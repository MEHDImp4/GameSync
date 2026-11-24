use tauri::{
    Webview,
    webview::{PageLoadEvent, PageLoadPayload},
};

pub fn page_loaded(webview: &Webview, page: &PageLoadPayload<'_>) {
    if page.event() != PageLoadEvent::Finished || webview.label() != "main" {
        return;
    }
    let Ok(input) = std::env::var("GAMESYNC_UI_SMOKE_INPUT") else {
        return;
    };
    let script = include_str!("../../tests/smoke.js");
    let input = serde_json::to_string(&input).expect("JSON string encoding cannot fail");
    if let Err(error) = webview.eval(format!("(({script})(JSON.parse({input})))")) {
        eprintln!("UI smoke injection failed: {error}");
    }
}

#[tauri::command]
pub fn smoke_result(error: Option<String>) -> Result<(), String> {
    let file = std::env::var_os("GAMESYNC_UI_SMOKE_RESULT").ok_or("smoke run not configured")?;
    std::fs::write(
        file,
        serde_json::to_vec(&error).map_err(|error| error.to_string())?,
    )
    .map_err(|error| error.to_string())
}
