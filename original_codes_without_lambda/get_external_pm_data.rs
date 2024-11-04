// [Korea Environment Corporation]: Integration of Real-Time Measurement Information by Station API (측정소별 실시간 측정정보 조회 API 연동)
//  .route("/exteranl-pm", get(get_external_pm_data_handler))

use crate::{
    models::{
        error_responses::errors_def::ErrorResponseCode,
        handler_models::{
            common_response_models::common_response_models::rust_response,
            environment_handler_models::get_external_data_model::{
                Data, Meta, ResponseData, SubRegionInfo,
            },
        },
    },
    queries::environment_queries::get_external_pm_queries::{
        GET_ALL_SUB_REGION_ID_AND_PM_STATION_QUERY, UPSERT_EXTERNAL_PM_QUERY,
    },
    server_init_funcs::get_state::ServerState,
};
use axum::{extract::State, response::IntoResponse};
use chrono::{DateTime, FixedOffset, Timelike, Utc};
use reqwest::Client;
use serde_json::Value;
use std::sync::Arc;
use tracing::error;

use tokio::{sync::Semaphore, task::JoinHandle};

pub async fn get_external_pm_data_handler(
    State(state): State<Arc<ServerState>>,
) -> impl IntoResponse {
    let start = tokio::time::Instant::now();

    let mut error_list = Vec::new();
    let mut response_data: Vec<ResponseData> = Vec::new();

    let db_pool = state.pool.clone();

    let db_client = match db_pool.get().await {
        Ok(client) => client,
        Err(e) => {
            error!("Failed to get a client from pool: {:?}", e);
            return (ErrorResponseCode::CONNECTION_POOL_ERROR).into_response();
        }
    };

    let sub_region_list: Vec<SubRegionInfo> = match db_client
        .query(GET_ALL_SUB_REGION_ID_AND_PM_STATION_QUERY, &[])
        .await
    {
        Ok(rows) => rows
            .into_iter()
            .map(|row| SubRegionInfo {
                sub_region_id: row.get::<&str, i32>("sub_region_id"),
                pm_station: row.get::<&str, String>("pm_station"),
            })
            .collect(),
        Err(e) => {
            error!("GET_ALL_SUB_REGION_ID_AND_PM_STATION_QUERY failed: {:?}", e);
            return (ErrorResponseCode::GET_ALL_SUB_REGION_ID_AND_PM_STATION_QUERY).into_response();
        }
    };

    let http_client = Client::new();
    let semaphore = Arc::new(Semaphore::new(10)); // Limit concurrent requests

    let mut tasks: Vec<JoinHandle<(Vec<ResponseData>, Vec<String>)>> = Vec::new();

    for single_sub_region in sub_region_list {
        let permit = semaphore.clone().acquire_owned().await.unwrap();
        let http_client = http_client.clone();
        let db_pool = db_pool.clone(); // Clone the database pool
        let state = state.clone();
        let single_sub_region = single_sub_region.clone();

        let task: JoinHandle<(Vec<ResponseData>, Vec<String>)> = tokio::spawn(async move {
            let mut local_response_data = Vec::new();
            let mut local_error_list = Vec::new();

            // Obtain a database client from the pool inside the task
            let db_client = match db_pool.get().await {
                Ok(client) => client,
                Err(e) => {
                    local_error_list.push(format!("Failed to get db client: {:?}", e));
                    error!("Failed to get db client: {:?}", e);
                    drop(permit);
                    return (local_response_data, local_error_list);
                }
            };

            let params = [
                ("serviceKey", &state.air_quality_api_key),
                ("returnType", &"json".to_string()),
                ("numOfRows", &"1000".to_string()),
                ("pageNo", &"1".to_string()),
                ("stationName", &single_sub_region.pm_station),
                ("dataTerm", &"DAILY".to_string()),
                ("ver", &"1.0".to_string()),
            ];

            let res = http_client
                .get("http://apis.data.go.kr/B552584/ArpltnInforInqireSvc/getMsrstnAcctoRltmMesureDnsty")
                .query(&params)
                .send()
                .await;

            let res = match res {
                Ok(res) => res,
                Err(e) => {
                    local_error_list.push(format!(
                        "{} : Request failed: {:?}",
                        single_sub_region.pm_station, e
                    ));
                    error!(
                        "Could not send request for {}: {:?}",
                        single_sub_region.pm_station, e
                    );
                    drop(permit);
                    return (local_response_data, local_error_list);
                }
            };

            let res_status = res.status();
            let res_headers = res.headers().clone();

            let res_text = match res.text().await {
                Ok(res_text) => res_text,
                Err(e) => {
                    local_error_list.push(format!(
                        "{} : Failed to read response text: {:?}",
                        single_sub_region.pm_station, e
                    ));
                    error!(
                        "Could not extract result text for {}: {:?}",
                        single_sub_region.pm_station, e
                    );
                    drop(permit);
                    return (local_response_data, local_error_list);
                }
            };

            // Check response status code
            if !res_status.is_success() {
                local_error_list.push(format!(
                    "{} : Received non-success status code: {}\nHeaders: {:?}\nResponse text: {}",
                    single_sub_region.pm_station, res_status, res_headers, res_text
                ));
                error!(
                    "Non-success status code for {}: {}",
                    single_sub_region.pm_station, res_status
                );
                drop(permit);
                return (local_response_data, local_error_list);
            }

            let json_response: Value = match serde_json::from_str(&res_text) {
                Ok(json) => json,
                Err(e) => {
                    local_error_list.push(format!(
                        "{} : Failed to parse JSON response: {:?}\nResponse text: {}",
                        single_sub_region.pm_station, e, res_text
                    ));
                    error!(
                        "Failed to parse JSON response for {}: {:?}",
                        single_sub_region.pm_station, e
                    );
                    drop(permit);
                    return (local_response_data, local_error_list);
                }
            };

            // Check for API error messages
            if let Some(error_message) = json_response
                .get("response")
                .and_then(|res| res.get("header"))
                .and_then(|header| header.get("resultMsg"))
                .and_then(|msg| msg.as_str())
            {
                if error_message != "NORMAL_CODE" {
                    local_error_list.push(format!(
                        "{} : API returned an error: {}",
                        single_sub_region.pm_station, error_message
                    ));
                    error!(
                        "API returned an error for {}: {}",
                        single_sub_region.pm_station, error_message
                    );
                    drop(permit);
                    return (local_response_data, local_error_list);
                }
            }

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
                local_error_list.push(format!(
                    "{} : No data available in API response.",
                    single_sub_region.pm_station
                ));
                error!(
                    "No data available in API response for {}",
                    single_sub_region.pm_station
                );
                drop(permit);
                return (local_response_data, local_error_list);
            }

            match db_client
                .query_one(
                    UPSERT_EXTERNAL_PM_QUERY,
                    &[
                        &single_sub_region.sub_region_id,
                        &pm10_value,
                        &pm25_value,
                        &recorded_at_datetime_utc,
                    ],
                )
                .await
            {
                Ok(row) => {
                    local_response_data.push(ResponseData {
                        pm10Value: row.get::<&str, Option<f64>>("pm10"),
                        pm25Value: row.get::<&str, Option<f64>>("pm25"),
                        dataTime: Some(row.get::<&str, DateTime<Utc>>("recorded_at")),
                        requestedTime: row.get::<&str, DateTime<Utc>>("update_at"),
                        stationName: single_sub_region.pm_station.clone(),
                    });
                }
                Err(e) => {
                    error!(
                        "UPSERT_EXTERNAL_PM_QUERY failed for {}: {:?}",
                        single_sub_region.pm_station, e
                    );
                    local_error_list.push(format!(
                        "{} : Database query failed: {:?}",
                        single_sub_region.pm_station, e
                    ));
                }
            };

            drop(permit);
            (local_response_data, local_error_list)
        });

        tasks.push(task);
    }

    // Wait for all tasks to complete and collect results
    for task in tasks {
        match task.await {
            Ok((local_response_data, local_error_list_task)) => {
                response_data.extend(local_response_data);
                error_list.extend(local_error_list_task);
            }
            Err(e) => {
                error!("Task failed: {:?}", e);
                // Handle task failure as needed
            }
        }
    }

    let count = response_data.len();
    return rust_response(
        Data {
            responseData: response_data,
        },
        Meta {
            timeTaken: format!("{:?}", start.elapsed()),
            message: format!("SUCCESS: {}", count),
            errorList: error_list,
        },
    );
}

// pub async fn get_external_pm_data_handler(
//     State(state): State<Arc<ServerState>>,
// ) -> impl IntoResponse {
//     let start = tokio::time::Instant::now();

//     let mut error_list = Vec::new();
//     let mut response_data: Vec<ResponseData> = Vec::new();

//     let client = match state.pool.get().await {
//         Ok(client) => client,
//         Err(e) => {
//             error!("Failed to get a client from pool: {:?}", e);
//             return (ErrorResponseCode::CONNECTION_POOL_ERROR).into_response();
//         }
//     };

//     let sub_region_list: Vec<SubRegionInfo> = match client
//         .query(GET_ALL_SUB_REGION_ID_AND_PM_STATION_QUERY, &[])
//         .await
//     {
//         Ok(rows) => rows
//             .into_iter()
//             .map(|row| SubRegionInfo {
//                 sub_region_id: row.get::<&str, i32>("sub_region_id"),
//                 pm_station: row.get::<&str, String>("pm_station"),
//             })
//             .collect(),
//         Err(e) => {
//             error!("GET_ALL_SUB_REGION_ID_AND_PM_STATION_QUERY failed: {:?}", e);
//             return (ErrorResponseCode::GET_ALL_SUB_REGION_ID_AND_PM_STATION_QUERY).into_response();
//         }
//     };

//     let request_client = Client::new();

//     for single_sub_region in sub_region_list {
//         let params = [
//             ("serviceKey", &state.air_quality_api_key),
//             ("returnType", &"json".to_string()),
//             ("numOfRows", &"1000".to_string()),
//             ("pageNo", &"1".to_string()),
//             ("stationName", &single_sub_region.pm_station),
//             ("dataTerm", &"DAILY".to_string()),
//             ("ver", &"1.0".to_string()),
//         ];
//         let res_text = match request_client
//             .get(
//                 "http://apis.data.go.kr/B552584/ArpltnInforInqireSvc/getMsrstnAcctoRltmMesureDnsty",
//             )
//             .query(&params)
//             .send()
//             .await
//         {
//             Ok(res) => match res.text().await {
//                 Ok(res_text) => res_text,
//                 Err(e) => {
//                     error_list.push(single_sub_region.pm_station);
//                     error!("Could not extract result text from pm responses: {:?}", e);
//                     continue;
//                 }
//             },
//             Err(e) => {
//                 error_list.push(single_sub_region.pm_station);
//                 error!("Could not send request: {:?}", e);
//                 continue;
//             }
//         };

//         let json_response: Value = match serde_json::from_str(&res_text) {
//             Ok(json) => json,
//             Err(e) => {
//                 error_list.push(single_sub_region.pm_station.clone());
//                 error!("Failed to parse JSON response: {:?}", e);
//                 continue;
//             }
//         };

//         let latest_item = json_response
//             .get("response")
//             .and_then(|res| res.get("body"))
//             .and_then(|body| body.get("items"))
//             .and_then(|items| items.get(0));

//         let mut pm10_value: Option<f64> = None;
//         let mut pm25_value: Option<f64> = None;

//         let mut recorded_at_datetime_utc = Utc::now();

//         if let Some(item) = latest_item {
//             // Only parse if `items` are available
//             pm10_value = item
//                 .get("pm10Value")
//                 .and_then(|v| v.as_str())
//                 .filter(|&v| v != "-")
//                 .and_then(|v| v.parse::<f64>().ok());

//             pm25_value = item
//                 .get("pm25Value")
//                 .and_then(|v| v.as_str())
//                 .filter(|&v| v != "-")
//                 .and_then(|v| v.parse::<f64>().ok());

//             // 0번째 데이터가 recoreded_at과 일치하면 다음 station으로 넘어감
//             let recorded_at = item.get("dataTime").and_then(|v| v.as_str()).unwrap_or("");

//             let kst_offset = FixedOffset::east_opt(9 * 3600).expect("Invalid offset");
//             let recorded_at_datetime_kst = DateTime::parse_from_str(recorded_at, "%Y-%m-%d %H:%M")
//                 .unwrap_or_else(|_| Utc::now().with_timezone(&kst_offset)); // Parse as KST

//             // UTC로 변환
//             recorded_at_datetime_utc = recorded_at_datetime_kst.with_timezone(&Utc);
//             recorded_at_datetime_utc = recorded_at_datetime_utc
//                 .with_minute(0)
//                 .unwrap()
//                 .with_second(0)
//                 .unwrap()
//                 .with_nanosecond(0)
//                 .unwrap();
//         }

//         match client
//             .query_one(
//                 UPSERT_EXTERNAL_PM_QUERY,
//                 &[
//                     &single_sub_region.sub_region_id,
//                     &pm10_value,
//                     &pm25_value,
//                     &recorded_at_datetime_utc,
//                 ],
//             )
//             .await
//         {
//             Ok(row) => response_data.push(ResponseData {
//                 pm10Value: row.get::<&str, Option<f64>>("pm10"),
//                 pm25Value: row.get::<&str, Option<f64>>("pm25"),
//                 dataTime: Some(row.get::<&str, DateTime<Utc>>("recorded_at")),
//                 requestedTime: row.get::<&str, DateTime<Utc>>("update_at"),
//                 stationName: single_sub_region.pm_station,
//             }),
//             Err(e) => {
//                 error!("UPSERT_EXTERNAL_PM_QUERY failed: {:?}", e);
//                 error_list.push(single_sub_region.pm_station);
//                 continue;
//             }
//         };
//     }
//     let count = &response_data.len();
//     return rust_response(
//         Data {
//             responseData: response_data,
//         },
//         Meta {
//             timeTaken: format!("{:?}", start.elapsed()),
//             message: format!("SUCCESS: {}", count),
//             errorList: error_list,
//         },
//     );
// }
