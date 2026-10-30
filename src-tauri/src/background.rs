//! One bounded display background, stored outside the music library.
use lofty::picture::PictureInformation;
use serde::{Deserialize, Serialize};

const IMAGE_LIMIT: usize = 1024 * 1024;
const STATE_LIMIT: usize = IMAGE_LIMIT * 4 + 128;
#[derive(Serialize, Deserialize)]
struct Background {
    version: u8,
    bytes: Vec<u8>,
}
fn validate(bytes: &[u8]) -> Result<(), String> {
    if bytes.is_empty() || bytes.len() > IMAGE_LIMIT {
        return Err("Choose an image smaller than 1 MB after resizing".into());
    }
    let info = PictureInformation::from_jpeg(bytes)
        .map_err(|_| "Background must be a JPEG image".to_string())?;
    if info.width == 0 || info.height == 0 || info.width > 1920 || info.height > 1920 {
        return Err("Background dimensions must be between 1 and 1920 pixels".into());
    }
    Ok(())
}
#[tauri::command]
pub async fn player_background() -> Result<tauri::ipc::Response, String> {
    tauri::async_runtime::spawn_blocking(|| {
        let path = crate::state::directory()?.join("player-background.json");
        let saved = crate::state::load::<Background>(&path, STATE_LIMIT)?;
        let bytes = match saved {
            Some(saved) if saved.version == 1 && saved.bytes.is_empty() => Vec::new(),
            Some(saved) if saved.version == 1 => {
                validate(&saved.bytes)?;
                saved.bytes
            }
            Some(_) => return Err("Saved background version is unsupported".to_string()),
            None => Vec::new(),
        };
        Ok(tauri::ipc::Response::new(bytes))
    })
    .await
    .map_err(|_| "Background reader stopped".to_string())?
}
fn save(bytes: Vec<u8>) -> Result<(), String> {
    let path = crate::state::directory()?.join("player-background.json");
    let data = crate::state::encode(&Background { version: 1, bytes }, STATE_LIMIT)?;
    crate::state::replace(&path, &data)
}
#[tauri::command]
pub async fn set_player_background(bytes: Vec<u8>) -> Result<(), String> {
    validate(&bytes)?;
    tauri::async_runtime::spawn_blocking(move || save(bytes))
        .await
        .map_err(|_| "Background writer stopped".to_string())?
}
#[tauri::command]
pub async fn clear_player_background() -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(|| save(Vec::new()))
        .await
        .map_err(|_| "Background writer stopped".to_string())?
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rejects_non_images_and_oversized_backgrounds() {
        assert!(validate(b"not an image").is_err());
        assert!(validate(&vec![0; IMAGE_LIMIT + 1]).is_err());
        assert!(validate(include_bytes!("../icons/128x128.png")).is_err());
    }
}
