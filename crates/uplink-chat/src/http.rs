use crate::adapter::ChatFehler;
pub async fn bytes(mut response: reqwest::Response) -> Result<Vec<u8>, ChatFehler> {
    const MAX: usize = 1024 * 1024;
    if response.content_length().is_some_and(|n| n > MAX as u64) {
        return Err(ChatFehler::Netz("Plattformantwort ist zu groß".into()));
    }
    let mut body = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|_| ChatFehler::Netz("Plattformantwort ist nicht lesbar".into()))?
    {
        if body.len().saturating_add(chunk.len()) > MAX {
            return Err(ChatFehler::Netz("Plattformantwort ist zu groß".into()));
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}
pub async fn json<T: serde::de::DeserializeOwned>(r: reqwest::Response) -> Result<T, ChatFehler> {
    serde_json::from_slice(&bytes(r).await?)
        .map_err(|_| ChatFehler::Netz("Plattformantwort ist nicht lesbar".into()))
}
