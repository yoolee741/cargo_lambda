// src/state.rs

use anyhow::{anyhow, Result};
use deadpool_postgres::{Config, ManagerConfig, Pool, RecyclingMethod, Runtime};
use tokio_postgres::NoTls;
use tracing::info;

pub struct ServerState {
    pub pool: Pool,
    pub air_quality_api_key: String,
}

impl ServerState {
    pub fn new(pool: Pool, air_quality_api_key: String) -> Self {
        ServerState {
            pool,
            air_quality_api_key,
        }
    }
}

// ServerState 초기화 함수
pub async fn initialize_state(db_conn_url: &str, air_quality_api_key: &str) -> Result<ServerState> {
    // 데이터베이스 풀 설정
    let mut cfg = Config::new();
    cfg.url = Some(db_conn_url.to_owned());
    cfg.manager = Some(ManagerConfig {
        recycling_method: RecyclingMethod::Fast,
    });

    let pool = cfg
        .create_pool(Some(Runtime::Tokio1), NoTls)
        .map_err(|e| anyhow!("Pool 생성 실패: {:?}", e))?;
    info!("Connection pool established.");

    Ok(ServerState::new(pool, air_quality_api_key.to_owned()))
}
