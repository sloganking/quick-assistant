use anyhow::{Context, Result};
use reqwest::Client;
use serde_json::json;

/// Searches the web using a temporary OpenAI assistant with browsing enabled.
/// The query is passed to the assistant and the assistant's final reply is returned.
/// Requires the `OPENAI_API_KEY` environment variable to be set.
pub async fn web_search(query: &str) -> Result<String> {
    let api_key = std::env::var("OPENAI_API_KEY")
        .context("OPENAI_API_KEY environment variable not set")?;

    let client = Client::new();

    // Create a temporary assistant with the browser tool enabled
    let assistant_res: serde_json::Value = client
        .post("https://api.openai.com/v1/assistants")
        .bearer_auth(&api_key)
        .json(&json!({
            "model": "gpt-4o", // default model with browsing
            "instructions": "Answer user questions using web search.",
            "tools": [{"type": "browser"}],
        }))
        .send()
        .await
        .context("Failed to create assistant")?
        .json()
        .await
        .context("Failed to parse assistant response")?;

    let assistant_id = assistant_res["id"].as_str().context("No assistant id")?.to_string();

    // Create a thread
    let thread_res: serde_json::Value = client
        .post("https://api.openai.com/v1/threads")
        .bearer_auth(&api_key)
        .json(&json!({}))
        .send()
        .await
        .context("Failed to create thread")?
        .json()
        .await
        .context("Failed to parse thread response")?;

    let thread_id = thread_res["id"].as_str().context("No thread id")?.to_string();

    // Add user message
    client
        .post(&format!(
            "https://api.openai.com/v1/threads/{}/messages",
            thread_id
        ))
        .bearer_auth(&api_key)
        .json(&json!({"role": "user", "content": query}))
        .send()
        .await
        .context("Failed to add message")?;

    // Start the run
    let run_res: serde_json::Value = client
        .post(&format!(
            "https://api.openai.com/v1/threads/{}/runs",
            thread_id
        ))
        .bearer_auth(&api_key)
        .json(&json!({"assistant_id": assistant_id}))
        .send()
        .await
        .context("Failed to start run")?
        .json()
        .await
        .context("Failed to parse run response")?;

    let run_id = run_res["id"].as_str().context("No run id")?.to_string();

    // Poll the run status
    loop {
        let status_res: serde_json::Value = client
            .get(&format!(
                "https://api.openai.com/v1/threads/{}/runs/{}",
                thread_id, run_id
            ))
            .bearer_auth(&api_key)
            .send()
            .await
            .context("Failed to fetch run status")?
            .json()
            .await
            .context("Failed to parse run status")?;

        match status_res["status"].as_str() {
            Some("completed") => break,
            Some("failed") => return Err(anyhow::anyhow!("run failed")),
            _ => tokio::time::sleep(std::time::Duration::from_secs(1)).await,
        }
    }

    // Fetch messages and return the last assistant response
    let messages_res: serde_json::Value = client
        .get(&format!(
            "https://api.openai.com/v1/threads/{}/messages",
            thread_id
        ))
        .bearer_auth(&api_key)
        .send()
        .await
        .context("Failed to fetch messages")?
        .json()
        .await
        .context("Failed to parse messages")?;

    let messages = messages_res["data"].as_array().context("No messages array")?;
    let response = messages
        .iter()
        .filter(|m| m["role"] == "assistant")
        .max_by_key(|m| m["created_at"].as_i64().unwrap_or(0))
        .and_then(|m| m["content"][0]["text"]["value"].as_str())
        .unwrap_or("")
        .to_string();

    Ok(response)
}

