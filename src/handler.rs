// src/handler.rs

use chrono::{DateTime, FixedOffset, Timelike, Utc};
use lambda_runtime::{Error, LambdaEvent};
use serde_json::json;
use std::sync::Arc;
use tracing::error;

use crate::state::{initialize_state, ServerState};
use anyhow::Result;

use deadpool_postgres::Client as DbClient;
use reqwest::Client;

// SQL 쿼리 상수
pub const GET_ALL_SUB_REGION_ID_AND_PM_STATION_QUERY: &str = r#"
SELECT sub_region_id, pm_station
FROM v3.sub_region;
"#;

pub const UPSERT_EXTERNAL_PM_QUERY: &str = r#"
INSERT INTO v3.external_pm (sub_region_id, pm10, pm25, recorded_at)
VALUES ($1, $2, $3, $4)
ON CONFLICT (sub_region_id) 
DO UPDATE SET 
    pm10 = EXCLUDED.pm10,
    pm25 = EXCLUDED.pm25,
    recorded_at = EXCLUDED.recorded_at,
    update_at = now()
RETURNING *;
"#;

// AWS Lambda 핸들러 함수
pub async fn lambda_handler(
    event: LambdaEvent<serde_json::Value>,
) -> Result<serde_json::Value, Error> {
    let payload = event.payload;
    println!("Received event: {:?}", payload);

    // 환경 변수 로드
    let db_conn_url = std::env::var("DB_CONN_URL")
        .map_err(|e| anyhow::anyhow!("DB_CONN_URL 환경 변수 누락: {:?}", e))?;
    let air_quality_api_key = std::env::var("AIR_QUALITY_API_KEY")
        .map_err(|e| anyhow::anyhow!("AIR_QUALITY_API_KEY 환경 변수 누락: {:?}", e))?;

    // ServerState 초기화
    let state = initialize_state(&db_conn_url, &air_quality_api_key)
        .await
        .map_err(|e| anyhow::anyhow!("ServerState 초기화 실패: {:?}", e))?;

    let state = Arc::new(state);

    // 외부 API 호출 및 데이터베이스 저장 로직
    match get_external_pm_data_handler(state).await {
        Ok(response) => Ok(json!({
            "statusCode": 200,
            "body": response,
        })),
        Err(e) => {
            error!("핸들러 실행 중 오류 발생: {:?}", e);
            Ok(json!({
                "statusCode": 500,
                "body": "Internal Server Error",
            }))
        }
    }
}

// 실제 핸들러 로직
async fn get_external_pm_data_handler(
    state: Arc<ServerState>,
) -> Result<serde_json::Value, anyhow::Error> {
    // 데이터베이스에서 필요한 정보 조회 (모든 측정소 ID 및 이름 가져오기)
    let db_client: DbClient = state.pool.get().await?;

    let rows = db_client
        .query(GET_ALL_SUB_REGION_ID_AND_PM_STATION_QUERY, &[])
        .await?;

    // 동시성 제어를 위한 세마포어 설정
    let semaphore = Arc::new(tokio::sync::Semaphore::new(10)); // 동시 요청 제한
    let http_client = Client::new();

    let mut tasks = Vec::new();
    let mut response_data = Vec::new();
    let mut error_list = Vec::new();

    for row in rows {
        let sub_region_id: i32 = row.get("sub_region_id");
        let pm_station: String = row.get("pm_station");

        let semaphore = semaphore.clone();
        let http_client = http_client.clone();
        let state = state.clone();

        let task = tokio::spawn(async move {
            // 각 태스크 내에서 응답 데이터와 에러 리스트를 초기화
            let mut local_response_data = Vec::new();
            let mut local_error_list = Vec::new();

            // 세마포어 퍼밋 획득
            let _permit = semaphore.acquire_owned().await.unwrap();

            // 새로운 DB 클라이언트 획득
            let db_client = match state.pool.get().await {
                Ok(client) => client,
                Err(e) => {
                    let error_message =
                        format!("{} : Failed to get db client: {:?}", pm_station, e);
                    error!("{}", error_message);
                    return (local_response_data, vec![error_message]);
                }
            };

            // 외부 API 호출 파라미터 설정
            let params = [
                ("serviceKey", &state.air_quality_api_key),
                ("returnType", &"json".to_string()),
                ("numOfRows", &"1000".to_string()),
                ("pageNo", &"1".to_string()),
                ("stationName", &pm_station),
                ("dataTerm", &"DAILY".to_string()),
                ("ver", &"1.0".to_string()),
            ];

            // 외부 API 호출
            let res = match http_client
                .get("http://apis.data.go.kr/B552584/ArpltnInforInqireSvc/getMsrstnAcctoRltmMesureDnsty")
                .query(&params)
                .send()
                .await
            {
                Ok(response) => response,
                Err(e) => {
                    let error_message = format!("{} : Request failed: {:?}", pm_station, e);
                    error!("{}", error_message);
                    local_error_list.push(error_message);
                    return (local_response_data, local_error_list);
                }
            };

            // 응답 상태 코드 확인
            if !res.status().is_success() {
                let res_status = res.status();
                let res_headers = res.headers().clone();
                let res_text = res.text().await.unwrap_or_default();
                let error_message = format!(
                    "{} : Received non-success status code: {}\nHeaders: {:?}\nResponse text: {}",
                    pm_station, res_status, res_headers, res_text
                );
                error!("{}", error_message);
                local_error_list.push(error_message);
                return (local_response_data, local_error_list);
            }

            // JSON 응답 파싱을 위해 응답 본문을 텍스트로 먼저 읽기
            let res_text = match res.text().await {
                Ok(text) => text,
                Err(e) => {
                    let error_message =
                        format!("{} : Failed to read response text: {:?}", pm_station, e);
                    error!("{}", error_message);
                    local_error_list.push(error_message);
                    return (local_response_data, local_error_list);
                }
            };

            // 텍스트를 JSON으로 파싱
            let json_response: serde_json::Value = match serde_json::from_str(&res_text) {
                Ok(json) => json,
                Err(e) => {
                    let error_message = format!(
                        "{} : Failed to parse JSON response: {:?}\nResponse text: {}",
                        pm_station, e, res_text
                    );
                    error!("{}", error_message);
                    local_error_list.push(error_message);
                    return (local_response_data, local_error_list);
                }
            };

            // API 응답에서 에러 메시지 확인
            if let Some(error_message) = json_response
                .get("response")
                .and_then(|res| res.get("header"))
                .and_then(|header| header.get("resultMsg"))
                .and_then(|msg| msg.as_str())
            {
                if error_message != "NORMAL_CODE" {
                    let error_message =
                        format!("{} : API returned an error: {}", pm_station, error_message);
                    error!("{}", error_message);
                    local_error_list.push(error_message);
                    return (local_response_data, local_error_list);
                }
            }

            // 최신 데이터 추출
            let latest_item = json_response
                .get("response")
                .and_then(|res| res.get("body"))
                .and_then(|body| body.get("items"))
                .and_then(|items| items.get(0));

            let mut pm10_value: Option<f64> = None;
            let mut pm25_value: Option<f64> = None;
            let mut recorded_at_datetime_utc = Utc::now();

            if let Some(item) = latest_item {
                pm10_value = item
                    .get("pm10Value")
                    .and_then(|v| v.as_str())
                    .filter(|&v| v != "-")
                    .and_then(|v| v.parse::<f64>().ok());

                pm25_value = item
                    .get("pm25Value")
                    .and_then(|v| v.as_str())
                    .filter(|&v| v != "-")
                    .and_then(|v| v.parse::<f64>().ok());

                let recorded_at = item.get("dataTime").and_then(|v| v.as_str()).unwrap_or("");

                let kst_offset = FixedOffset::east_opt(9 * 3600).expect("Invalid offset");
                let recorded_at_datetime_kst =
                    DateTime::parse_from_str(recorded_at, "%Y-%m-%d %H:%M")
                        .unwrap_or_else(|_| Utc::now().with_timezone(&kst_offset));

                recorded_at_datetime_utc = recorded_at_datetime_kst.with_timezone(&Utc);
                recorded_at_datetime_utc = recorded_at_datetime_utc
                    .with_minute(0)
                    .unwrap()
                    .with_second(0)
                    .unwrap()
                    .with_nanosecond(0)
                    .unwrap();
            } else {
                let error_message = format!("{} : No data available in API response.", pm_station);
                error!("{}", error_message);
                local_error_list.push(error_message);
                return (local_response_data, local_error_list);
            }

            // 데이터베이스에 upsert
            match db_client
                .query_one(
                    UPSERT_EXTERNAL_PM_QUERY,
                    &[
                        &sub_region_id,
                        &pm10_value,
                        &pm25_value,
                        &recorded_at_datetime_utc,
                    ],
                )
                .await
            {
                Ok(row) => {
                    local_response_data.push(json!({
                        "pm10Value": row.get::<&str, Option<f64>>("pm10"),
                        "pm25Value": row.get::<&str, Option<f64>>("pm25"),
                        "dataTime": row.get::<&str, DateTime<Utc>>("recorded_at"),
                        "requestedTime": row.get::<&str, DateTime<Utc>>("update_at"),
                        "stationName": pm_station.clone(),
                    }));
                }
                Err(e) => {
                    let error_message = format!("{} : Database query failed: {:?}", pm_station, e);
                    error!("{}", error_message);
                    local_error_list.push(error_message);
                }
            }

            (local_response_data, local_error_list)
        });

        tasks.push(task);
    }

    // 모든 태스크 완료 대기 및 결과 수집
    for task in tasks {
        match task.await {
            Ok((local_response_data, local_error_list_task)) => {
                response_data.extend(local_response_data);
                error_list.extend(local_error_list_task);
            }
            Err(e) => {
                error!("Task failed: {:?}", e);
            }
        }
    }

    // 최종 응답 구성
    Ok(json!({
        "data": response_data,
        "meta": {
            "message": format!("SUCCESS: {}", response_data.len()),
            "errorList": error_list,
        }
    }))
}
