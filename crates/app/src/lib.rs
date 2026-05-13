#[tauri::command]
async fn ping(name: String) -> Result<String, String> {
    Ok(format!("pong, {name}"))
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .invoke_handler(tauri::generate_handler![ping])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn ping_returns_formatted_pong() {
        let reply = ping("world".to_string()).await.unwrap();
        assert_eq!(reply, "pong, world");
    }
}
