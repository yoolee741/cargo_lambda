// src/main.rs

use lambda_runtime::{service_fn, Error};
use tracing_subscriber::EnvFilter;

mod handler;
mod state;

#[tokio::main]
async fn main() -> Result<(), Error> {
    // 로깅 초기화
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env())
        .init();

    // Lambda 핸들러 설정
    let func = service_fn(handler::lambda_handler);

    // Lambda 함수 실행
    lambda_runtime::run(func).await?;
    Ok(())
}
