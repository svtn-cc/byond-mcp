//! MCP server (JSON-RPC over stdio) that builds, runs and drives a BYOND world for testing.

mod app;
mod process;
#[cfg(windows)]
mod screenshot;
mod tools;
mod topic;

use std::io::{self, BufRead, Write};
use std::path::PathBuf;

use anyhow::{Context, Result};
use serde_json::{json, Value};

use app::App;

const DEFAULT_PROTOCOL: &str = "2024-11-05";

fn main() -> Result<()> {
	let mut app = App::new(project_dir()?)?;
	let mut stdout = io::stdout().lock();
	for line in io::stdin().lock().lines() {
		let line = line?;
		if line.trim().is_empty() {
			continue;
		}
		let reply = match serde_json::from_str::<Value>(&line) {
			Ok(message) => handle(&mut app, &message),
			Err(error) => Some(json!({
				"jsonrpc": "2.0",
				"id": null,
				"error": { "code": -32700, "message": error.to_string() },
			})),
		};
		if let Some(reply) = reply {
			writeln!(stdout, "{reply}")?;
			stdout.flush()?;
		}
	}
	// The client is gone: nothing may outlive it, least of all a world with the harness open.
	app.shutdown();
	Ok(())
}

/// `--project <dir>`, else `BYOND_MCP_PROJECT`, else the working directory.
fn project_dir() -> Result<PathBuf> {
	let mut args = std::env::args().skip(1);
	while let Some(arg) = args.next() {
		if arg == "--project" {
			return args.next().map(PathBuf::from).context("--project needs a directory");
		}
	}
	match std::env::var_os("BYOND_MCP_PROJECT") {
		Some(dir) => Ok(PathBuf::from(dir)),
		None => Ok(std::env::current_dir()?),
	}
}

/// Answers one JSON-RPC message; notifications get no reply.
fn handle(app: &mut App, message: &Value) -> Option<Value> {
	let id = message.get("id")?.clone();
	let params = &message["params"];
	let outcome = match message["method"].as_str().unwrap_or_default() {
		"initialize" => Ok(json!({
			"protocolVersion": params["protocolVersion"].as_str().unwrap_or(DEFAULT_PROTOCOL),
			"capabilities": { "tools": {} },
			"serverInfo": { "name": "byond-mcp", "version": env!("CARGO_PKG_VERSION") },
		})),
		"ping" => Ok(json!({})),
		"tools/list" => Ok(json!({ "tools": tools::definitions() })),
		"tools/call" => Ok(call_tool(app, params)),
		other => Err(format!("unknown method {other}")),
	};
	Some(match outcome {
		Ok(result) => json!({ "jsonrpc": "2.0", "id": id, "result": result }),
		Err(message) => json!({ "jsonrpc": "2.0", "id": id, "error": { "code": -32601, "message": message } }),
	})
}

fn call_tool(app: &mut App, params: &Value) -> Value {
	let name = params["name"].as_str().unwrap_or_default();
	match app.call(name, &params["arguments"]) {
		Ok(content) => json!({ "content": content, "isError": false }),
		Err(error) => json!({
			"content": [{ "type": "text", "text": format!("{error:#}") }],
			"isError": true,
		}),
	}
}
