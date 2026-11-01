use futures_util::{
    future::{select, Either},
    StreamExt,
};
use std::{future::Future, time::Duration};
use worker::*;

/// Bounds the complete fetch, including response-body consumption; abort on timeout.
pub async fn fetch_json(req: Request, timeout_secs: u64) -> Result<(u16, serde_json::Value)> {
    let controller = AbortController::default();
    let signal = controller.signal();
    let operation = async {
        let mut resp = Fetch::Request(req).send_with_signal(&signal).await?;
        let status = resp.status_code();
        if status == 204 || status == 205 {
            return Ok((status, serde_json::Value::Null));
        }
        if status == 204 || status == 205 {
            return Ok((status, serde_json::Value::Null));
        }
        let mut bytes = Vec::new();
        let mut stream = resp.stream()?;
        while let Some(chunk) = stream.next().await {
            let chunk = chunk?;
            if bytes.len() + chunk.len() > 2 * 1024 * 1024 {
                return Err(Error::from("upstream response exceeds 2 MiB"));
            }
            bytes.extend_from_slice(&chunk);
        }
        let value = if bytes.is_empty() {
            serde_json::Value::Null
        } else {
            serde_json::from_slice(&bytes)
                .map_err(|_| Error::from("upstream returned invalid JSON"))?
        };
        Ok((status, value))
    };
    match timeout(operation, timeout_secs).await {
        Ok(result) => result,
        Err(()) => {
            controller.abort();
            Err(Error::from("upstream request timed out"))
        }
    }
}

pub async fn timeout<F: Future>(future: F, seconds: u64) -> std::result::Result<F::Output, ()> {
    let timer = Delay::from(Duration::from_secs(seconds));
    futures_util::pin_mut!(future, timer);
    match select(future, timer).await {
        Either::Left((result, _)) => Ok(result),
        Either::Right(_) => Err(()),
    }
}
