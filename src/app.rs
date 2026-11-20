//! Tool implementations: build, world lifecycle, clients and harness calls.

use std::collections::hash_map::RandomState;
use std::collections::HashMap;
use std::fs;
use std::hash::{BuildHasher, Hasher};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::{anyhow, bail, Context, Result};
use serde_json::{json, Map, Value};

use crate::process::Captured;
use crate::topic;

const HARNESS_SOURCE: &str = include_str!("../dm/mcp_harness.dm");
/// Requests above this size travel through a file: a topic packet tops out at 64 KiB.
const INLINE_REQUEST_LIMIT: usize = 30_000;
const TOPIC_TIMEOUT: Duration = Duration::from_secs(15);
const POLL: Duration = Duration::from_millis(200);

/// How long the pager needs after start before Dream Seeker can obtain the account's key from it.
const PAGER_LOGIN_DELAY: Duration = Duration::from_secs(12);
/// A full codebase streams its resources to a fresh client before it logs in.
const CLIENT_LOGIN_WAIT: Duration = Duration::from_secs(240);

pub struct App {
	project: PathBuf,
	/// `<project>/<stem>.dme`
	dme: PathBuf,
	byond_bin: PathBuf,
	build: Option<Captured>,
	server: Option<Server>,
	clients: Vec<Client>,
	log_offsets: HashMap<String, usize>,
	request_seq: u32,
}

/// Kept after the process ends so its logs stay readable.
struct Server {
	process: Captured,
	port: u16,
	token: String,
	log_dir: String,
}

struct Client {
	child: Child,
	ckey: String,
}

impl App {
	pub fn new(project: PathBuf) -> Result<Self> {
		// Not canonicalize(): its \\?\ form on Windows is not something to hand to BYOND's tools.
		let project = std::path::absolute(&project)?;
		if !project.is_dir() {
			bail!("project directory {project:?} not found");
		}
		Ok(Self {
			dme: find_dme(&project)?,
			byond_bin: find_byond_bin()?,
			project,
			build: None,
			server: None,
			clients: Vec::new(),
			log_offsets: HashMap::new(),
			request_seq: 0,
		})
	}

	pub fn shutdown(&mut self) {
		for mut client in self.clients.drain(..) {
			let _ = client.child.kill();
		}
		if let Some(server) = &mut self.server {
			server.process.kill();
		}
		if let Some(build) = &mut self.build {
			build.kill();
			self.remove_build_inputs();
		}
	}

	/// Runs a tool and returns MCP content blocks.
	pub fn call(&mut self, name: &str, args: &Value) -> Result<Value> {
		let wait = Duration::from_secs_f64(args["wait_secs"].as_f64().unwrap_or(20.0).max(0.0));
		let result = match name {
			"build" => self.build(args, wait)?,
			"server_start" => self.server_start(args, wait)?,
			"server_stop" => {
				self.shutdown();
				json!({ "stopped": true })
			}
			"server_status" => self.server_status()?,
			"logs" => self.logs(args)?,
			"client_join" => self.client_join(args, wait)?,
			"client_close" => self.client_close(args["ckey"].as_str()),
			"clients" => self.harness(json!({ "op": "clients" }), wait)?,
			"client_command" => self.harness(
				json!({ "op": "command", "ckey": text(args, "ckey")?, "command": text(args, "command")? }),
				wait,
			)?,
			"call_proc" => self.harness(
				json!({
					"op": "call",
					"proc": text(args, "proc")?,
					"target": args["target"],
					"args": args.get("args").cloned().unwrap_or_else(|| json!([])),
					"as": args["as_ckey"],
					"depth": args["depth"],
				}),
				wait,
			)?,
			"inspect" => self.harness(
				json!({ "op": "get", "target": args["target"], "vars": args["vars"], "depth": args["depth"] }),
				wait,
			)?,
			"set_var" => self.harness(
				json!({ "op": "set", "target": args["target"], "var": text(args, "var")?, "value": args["value"] }),
				wait,
			)?,
			"spawn" => self.harness(
				json!({
					"op": "new",
					"path": text(args, "path")?,
					"loc": args["loc"],
					"args": args.get("args").cloned().unwrap_or_else(|| json!([])),
				}),
				wait,
			)?,
			"delete" => self.harness(json!({ "op": "del", "target": args["target"] }), wait)?,
			"find" => self.harness(
				json!({ "op": "find", "path": text(args, "path")?, "name": args["name"], "limit": args["limit"] }),
				wait,
			)?,
			"eval_lua" => self.harness(
				json!({
					"op": "lua",
					"script": text(args, "script")?,
					"as": args["as_ckey"],
					"depth": args["depth"].as_u64().unwrap_or(4),
				}),
				wait,
			)?,
			"job_result" => self.harness(json!({ "op": "job", "id": args["job"] }), wait)?,
			"enter_game" => self.enter_game(args)?,
			"ui_list" => self.harness(
				json!({ "op": "ui_list", "ckey": text(args, "ckey")?, "depth": args["depth"].as_u64().unwrap_or(5) }),
				wait,
			)?,
			"ui_act" => {
				let mut request = args.clone();
				request["op"] = "ui_act".into();
				self.harness(request, wait)?
			}
			"screenshot" => return self.screenshot(args),
			other => bail!("unknown tool {other}"),
		};
		Ok(json!([{ "type": "text", "text": serde_json::to_string_pretty(&result)? }]))
	}

	// ---- build ----

	fn stem(&self) -> String {
		self.dme.file_stem().unwrap_or_default().to_string_lossy().into_owned()
	}

	fn harness_dmb(&self) -> PathBuf {
		self.project.join(format!("{}.mcp.dmb", self.stem()))
	}

	/// The two files that exist only while the compiler runs.
	fn build_inputs(&self) -> [PathBuf; 2] {
		let stem = self.stem();
		[self.project.join(format!("{stem}.mcp.dme")), self.project.join(format!("{stem}.mcp_harness.dm"))]
	}

	fn remove_build_inputs(&self) {
		for path in self.build_inputs() {
			let _ = fs::remove_file(path);
		}
	}

	fn build(&mut self, args: &Value, wait: Duration) -> Result<Value> {
		if self.build.is_none() {
			if self.server_running() {
				bail!("the world is running from the harness build; call server_stop before rebuilding");
			}
			let [environment, harness] = self.build_inputs();
			let mut source = fs::read_to_string(&self.dme).with_context(|| format!("cannot read {:?}", self.dme))?;
			// Appended last so the harness can see every define and override world/Topic on top of the codebase's.
			source.push_str(&format!("\n#include \"{}\"\n", harness.file_name().unwrap().to_string_lossy()));
			fs::write(&environment, source)?;
			fs::write(&harness, HARNESS_SOURCE)?;

			let mut command = Command::new(self.byond_bin.join(exe("dm", "DreamMaker")));
			command.current_dir(&self.project).arg("-DMCP_HARNESS");
			if self.project.join("tools/build/build.ts").exists() {
				command.arg("-DCBT"); // tgstation-family codebases warn when built outside their build tool
			}
			for define in args["defines"].as_array().into_iter().flatten() {
				command.arg(format!("-D{}", define.as_str().context("defines must be strings")?));
			}
			if let Some(map) = args["map"].as_str() {
				command.arg(format!("-DFORCE_MAP=\"{map}\""));
			}
			command.arg(&environment);
			self.build = Some(Captured::spawn(command)?);
		}

		let build = self.build.as_mut().unwrap();
		let Some(code) = build.wait_for(wait) else {
			return Ok(json!({
				"status": "running",
				"elapsed_secs": build.started.elapsed().as_secs(),
				"hint": "call build again to keep waiting",
			}));
		};
		let build = self.build.take().unwrap();
		self.remove_build_inputs();

		let output = build.output();
		let errors: Vec<&str> = output.lines().filter(|line| line.contains(":error")).take(50).collect();
		let warnings = output.lines().filter(|line| line.contains(":warning")).count();
		let succeeded = code == 0 && errors.is_empty() && self.harness_dmb().exists();
		Ok(json!({
			"status": if succeeded { "ok" } else { "failed" },
			"elapsed_secs": build.started.elapsed().as_secs(),
			"errors": errors,
			"warnings": warnings,
			"summary": output.lines().rev().take(3).collect::<Vec<_>>().into_iter().rev().collect::<Vec<_>>(),
			"dmb": self.harness_dmb(),
		}))
	}

	// ---- world lifecycle ----

	fn server_running(&mut self) -> bool {
		self.server.as_mut().is_some_and(|server| server.process.exit_code().is_none())
	}

	fn server_start(&mut self, args: &Value, wait: Duration) -> Result<Value> {
		if self.server_running() {
			bail!("the world is already running; call server_stop first");
		}
		let dmb = self.harness_dmb();
		if !dmb.exists() {
			bail!("{dmb:?} does not exist; call build first");
		}
		let port = match args["port"].as_u64() {
			Some(port) => u16::try_from(port).context("port out of range")?,
			None => TcpListener::bind(("127.0.0.1", 0))?.local_addr()?.port(),
		};
		let token = random_token();
		let log_dir = format!("mcp-{}", SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs());
		let mut params = format!("mcp_token={token}&log-directory={log_dir}");
		for (key, value) in args["params"].as_object().into_iter().flatten() {
			let value = value.as_str().map_or_else(|| value.to_string(), str::to_owned);
			params.push_str(&format!("&{}={}", topic::url_encode(key), topic::url_encode(&value)));
		}

		let daemon = ["dd", "dreamdaemon"]
			.map(|name| self.byond_bin.join(exe(name, "DreamDaemon")))
			.into_iter()
			.find(|path| path.exists())
			.context("DreamDaemon not found in the BYOND bin directory")?;
		let mut command = Command::new(daemon);
		command.current_dir(&self.project).arg(&dmb).arg(port.to_string()).args(["-trusted", "-close", "-params", &params]);
		self.log_offsets.clear();
		self.server = Some(Server { process: Captured::spawn(command)?, port, token, log_dir });

		let deadline = Instant::now() + wait;
		loop {
			if let Some(code) = self.server.as_mut().unwrap().process.exit_code() {
				bail!("DreamDaemon exited with code {code}:\n{}", self.server.as_ref().unwrap().process.output());
			}
			if let Ok(status) = self.exchange(&json!({ "op": "status" })) {
				return Ok(json!({ "status": "ready", "port": port, "world": unpair(status["result"].clone()) }));
			}
			if Instant::now() >= deadline {
				return Ok(json!({
					"status": "starting",
					"port": port,
					"hint": "the world has not answered yet; poll server_status",
				}));
			}
			thread::sleep(POLL);
		}
	}

	fn server_status(&mut self) -> Result<Value> {
		let Some(server) = &mut self.server else {
			return Ok(json!({ "running": false }));
		};
		if let Some(code) = server.process.exit_code() {
			return Ok(json!({ "running": false, "exit_code": code }));
		}
		let (pid, port) = (server.process.pid(), server.port);
		let world = match self.harness(json!({ "op": "status" }), Duration::ZERO) {
			Ok(world) => world,
			Err(error) => json!({ "unreachable": format!("{error:#}") }),
		};
		Ok(json!({ "running": true, "pid": pid, "port": port, "world": world }))
	}

	fn logs(&mut self, args: &Value) -> Result<Value> {
		let server = self.server.as_ref().context("no world has been started in this session")?;
		let dir = self.project.join("data/logs").join(&server.log_dir);
		let name = args["file"].as_str();
		let content = match name {
			None | Some("dd") => server.process.output(),
			Some(name) => {
				if Path::new(name).components().count() != 1 {
					bail!("file must be a bare name from the log listing");
				}
				String::from_utf8_lossy(&fs::read(dir.join(name)).with_context(|| format!("no log named {name}"))?)
					.into_owned()
			}
		};

		let key = name.unwrap_or("dd").to_owned();
		let mut text = content.as_str();
		if args["since_last"].as_bool() == Some(true) {
			let offset = self.log_offsets.get(&key).copied().unwrap_or(0);
			text = content.get(offset..).unwrap_or(text);
		}
		self.log_offsets.insert(key.clone(), content.len());

		let needle = args["grep"].as_str().map(str::to_lowercase);
		let mut lines: Vec<&str> =
			text.lines().filter(|line| needle.as_ref().is_none_or(|needle| line.to_lowercase().contains(needle))).collect();
		let tail = args["tail"].as_u64().unwrap_or(200) as usize;
		let skipped = lines.len().saturating_sub(tail);
		lines.drain(..skipped);

		let mut result = json!({ "file": key, "skipped_lines": skipped, "text": lines.join("\n") });
		if name.is_none() {
			let mut files: Vec<Value> = fs::read_dir(&dir)
				.into_iter()
				.flatten()
				.flatten()
				.filter_map(|entry| {
					let size = entry.metadata().ok().filter(|meta| meta.is_file())?.len();
					Some(json!({ "name": entry.file_name().to_string_lossy(), "bytes": size }))
				})
				.collect();
			files.sort_by_key(|file| file["name"].as_str().map(str::to_owned));
			result["files"] = files.into();
		}
		Ok(result)
	}

	// ---- clients ----

	fn connected_ckeys(&mut self) -> Result<Vec<String>> {
		let status = self.harness(json!({ "op": "status" }), Duration::ZERO)?;
		Ok(status["clients"].as_array().into_iter().flatten().filter_map(|ckey| ckey.as_str().map(str::to_owned)).collect())
	}

	/// BYOND offers exactly two identities on one machine: the account the pager is logged into, and a
	/// guest whose key is derived from the hardware id. Which one Dream Seeker gets depends solely on
	/// whether the pager is running when it starts; a client keeps its key after the pager exits.
	fn client_join(&mut self, args: &Value, wait: Duration) -> Result<Value> {
		let before = self.connected_ckeys()?;
		let as_account = match args["identity"].as_str().unwrap_or("guest") {
			"guest" => false,
			"account" => true,
			other => bail!("identity must be \"guest\" or \"account\", not {other:?}"),
		};
		if !as_account && before.iter().any(|ckey| ckey.starts_with("guest")) {
			bail!(
				"a guest is already connected, and every guest on this machine gets the same key, so a second \
				 one would replace it; join with identity \"account\" instead"
			);
		}

		let mut own_pager = None;
		match (as_account, pager_running()?) {
			(true, false) => {
				own_pager = Some(
					Command::new(self.byond_bin.join(exe("byond", "byond")))
						.stdin(Stdio::null())
						.stdout(Stdio::null())
						.stderr(Stdio::null())
						.spawn()
						.context("cannot start the BYOND pager")?,
				);
				thread::sleep(PAGER_LOGIN_DELAY);
			}
			(false, true) => bail!(
				"the BYOND pager is running, so Dream Seeker would log in as its account; close the pager for a \
				 guest, or join with identity \"account\""
			),
			_ => {}
		}
		let joined = self.launch_client(&before, wait);
		// Leaving our pager up would turn every later guest join into the account.
		if let Some(mut pager) = own_pager {
			let _ = pager.kill();
			let _ = pager.wait();
		}
		joined
	}

	fn launch_client(&mut self, before: &[String], wait: Duration) -> Result<Value> {
		let port = self.server.as_ref().context("the world is not running")?.port;
		let mut child = Command::new(self.byond_bin.join(exe("dreamseeker", "dreamseeker")))
			.arg(format!("byond://127.0.0.1:{port}"))
			.stdin(Stdio::null())
			.stdout(Stdio::null())
			.stderr(Stdio::null())
			.spawn()
			.context("cannot start Dream Seeker")?;

		let deadline = Instant::now() + wait;
		loop {
			if let Some(ckey) = self.connected_ckeys()?.into_iter().find(|ckey| !before.contains(ckey)) {
				let pid = child.id();
				self.clients.push(Client { child, ckey: ckey.clone() });
				return Ok(json!({ "ckey": ckey, "pid": pid }));
			}
			if let Ok(Some(status)) = child.try_wait() {
				bail!("Dream Seeker exited before logging in ({status})");
			}
			if Instant::now() >= deadline {
				let _ = child.kill();
				bail!(
					"no new client logged in within {} s. If the window logged in under a key that was already \
					 connected, it replaced that client instead; see logs",
					wait.as_secs()
				);
			}
			thread::sleep(POLL);
		}
	}

	/// Joins whichever of the two identities is still missing, then lets the world place everyone.
	fn enter_game(&mut self, args: &Value) -> Result<Value> {
		let wanted = args["players"].as_u64().unwrap_or(2);
		if wanted > 2 {
			bail!("one machine has two BYOND identities, so at most 2 players can be real windows");
		}
		let mut connected = self.connected_ckeys()?;
		for identity in ["guest", "account"] {
			if connected.len() as u64 >= wanted {
				break;
			}
			let present = connected.iter().any(|ckey| ckey.starts_with("guest") == (identity == "guest"));
			if present {
				continue;
			}
			self.client_join(&json!({ "identity": identity }), CLIENT_LOGIN_WAIT)?;
			connected = self.connected_ckeys()?;
		}

		let wait = Duration::from_secs_f64(args["wait_secs"].as_f64().unwrap_or(120.0).max(0.0));
		let mut request = args.clone();
		request["op"] = "enter".into();
		self.harness(request, wait)
	}

	fn client_close(&mut self, ckey: Option<&str>) -> Value {
		let (closing, kept): (Vec<Client>, Vec<Client>) =
			self.clients.drain(..).partition(|client| ckey.is_none_or(|ckey| client.ckey == ckey));
		self.clients = kept;
		let closed: Vec<String> = closing
			.into_iter()
			.map(|mut client| {
				let _ = client.child.kill();
				let _ = client.child.wait();
				client.ckey
			})
			.collect();
		json!({ "closed": closed })
	}

	#[cfg(windows)]
	fn screenshot(&mut self, args: &Value) -> Result<Value> {
		use base64::Engine;

		use crate::screenshot::{capture_png, windows_of};

		let client = match args["ckey"].as_str() {
			Some(ckey) => self.clients.iter().find(|client| client.ckey == ckey),
			None => self.clients.first(),
		}
		.context("no such client window; open one with client_join")?;
		let windows = windows_of(client.child.id());
		let titles: Vec<&str> = windows.iter().map(|window| window.title.as_str()).collect();
		let window = match args["window"].as_str() {
			Some(part) => windows.iter().find(|window| window.title.to_lowercase().contains(&part.to_lowercase())),
			None => windows.first(),
		}
		.with_context(|| format!("no matching window; open windows: {titles:?}"))?;
		let png = capture_png(window)?;
		Ok(json!([
			{
				"type": "text",
				"text": format!("{} ({}x{}); open windows: {titles:?}", window.title, window.width, window.height),
			},
			{ "type": "image", "data": base64::engine::general_purpose::STANDARD.encode(png), "mimeType": "image/png" },
		]))
	}

	#[cfg(not(windows))]
	fn screenshot(&mut self, _args: &Value) -> Result<Value> {
		bail!("screenshots are only implemented on Windows")
	}

	// ---- harness channel ----

	/// Runs one harness op, following it for up to `wait` if the world reports it is still running.
	fn harness(&mut self, request: Value, wait: Duration) -> Result<Value> {
		if !self.server_running() {
			bail!("the world is not running; call server_start");
		}
		let deadline = Instant::now() + wait;
		let mut reply = self.exchange(&request)?;
		while truthy(&reply["running"]) {
			if Instant::now() >= deadline {
				return Ok(json!({
					"status": "running",
					"job": reply["job"],
					"hint": "still running in the world; fetch it with job_result",
				}));
			}
			thread::sleep(POLL);
			reply = self.exchange(&json!({ "op": "job", "id": reply["job"] }))?;
		}
		Ok(unpair(reply["result"].take()))
	}

	/// One request/reply round trip. Fails with the world's own message when the op failed.
	fn exchange(&mut self, request: &Value) -> Result<Value> {
		let server = self.server.as_ref().context("the world is not running; call server_start")?;
		let body = topic::url_encode(&request.to_string());
		let payload = if body.len() <= INLINE_REQUEST_LIMIT {
			format!("data={body}")
		} else {
			self.request_seq += 1;
			let relative = format!("data/mcp_harness/request_{}.json", self.request_seq);
			let path = self.project.join(&relative);
			fs::create_dir_all(path.parent().unwrap())?;
			fs::write(&path, request.to_string())?;
			format!("file={}", topic::url_encode(&relative))
		};
		let query = format!("?mcp_harness=1&token={}&{payload}", server.token);

		let Some(text) = topic::send(server.port, &query, TOPIC_TIMEOUT)? else {
			bail!("the world answered without the harness; it must run the dmb produced by build");
		};
		let mut reply: Value = serde_json::from_str(&text).context("harness sent malformed JSON")?;
		if let Some(relative) = reply["file"].as_str() {
			let path = self.project.join(relative);
			reply = serde_json::from_str(&fs::read_to_string(&path)?).context("harness wrote malformed JSON")?;
			let _ = fs::remove_file(path);
		}
		if !truthy(&reply["ok"]) {
			return Err(anyhow!("{}", reply["error"].as_str().unwrap_or("harness reported an unspecified failure")));
		}
		Ok(reply)
	}
}

fn text<'a>(args: &'a Value, key: &str) -> Result<&'a str> {
	args[key].as_str().with_context(|| format!("missing string argument `{key}`"))
}

fn truthy(value: &Value) -> bool {
	value.as_bool().unwrap_or_else(|| value.as_f64().is_some_and(|number| number != 0.0))
}

/// The harness sends associative lists as `{"$pairs": [[key, value], ...]}`; fold them into objects.
fn unpair(value: Value) -> Value {
	match value {
		Value::Array(items) => Value::Array(items.into_iter().map(unpair).collect()),
		Value::Object(mut map) => {
			if map.len() == 1 {
				if let Some(Value::Array(pairs)) = map.remove("$pairs") {
					let folded: Map<String, Value> = pairs
						.into_iter()
						.filter_map(|pair| match pair {
							Value::Array(mut pair) if pair.len() == 2 => {
								let value = unpair(pair.pop()?);
								Some((pair.pop()?.as_str()?.to_owned(), value))
							}
							_ => None,
						})
						.collect();
					return Value::Object(folded);
				}
			}
			Value::Object(map.into_iter().map(|(key, value)| (key, unpair(value))).collect())
		}
		other => other,
	}
}

fn random_token() -> String {
	let mut token = String::new();
	for _ in 0..2 {
		// RandomState is seeded from the OS; hashing nothing yields the seed-dependent value.
		token.push_str(&format!("{:016x}", RandomState::new().build_hasher().finish()));
	}
	token
}

/// Whether the BYOND pager (byond.exe) is up; Dream Seeker takes its key from it when it is.
fn pager_running() -> Result<bool> {
	if !cfg!(windows) {
		return Ok(false);
	}
	let mut command = Command::new("tasklist");
	command.args(["/FI", "IMAGENAME eq byond.exe", "/FO", "CSV", "/NH"]);
	crate::process::hide_console(&mut command);
	let listing = command.output().context("cannot list processes")?;
	Ok(String::from_utf8_lossy(&listing.stdout).to_lowercase().contains("byond.exe"))
}

fn exe(windows: &str, unix: &str) -> String {
	if cfg!(windows) {
		format!("{windows}.exe")
	} else {
		unix.to_owned()
	}
}

/// The project's environment file: the only `.dme` that is not a test or harness variant.
fn find_dme(project: &Path) -> Result<PathBuf> {
	let mut found: Vec<PathBuf> = fs::read_dir(project)?
		.flatten()
		.map(|entry| entry.path())
		.filter(|path| path.extension().is_some_and(|ext| ext == "dme"))
		.filter(|path| {
			let name = path.file_name().unwrap_or_default().to_string_lossy();
			!name.contains(".test.") && !name.contains(".mcp.")
		})
		.collect();
	match found.len() {
		1 => Ok(found.remove(0)),
		0 => bail!("no .dme file in {project:?}; pass the project directory with --project"),
		_ => bail!("several .dme files in {project:?}: {found:?}; keep one or point --project elsewhere"),
	}
}

fn find_byond_bin() -> Result<PathBuf> {
	let compiler = exe("dm", "DreamMaker");
	let candidates = std::env::var_os("BYOND_BIN").map(PathBuf::from).into_iter().chain(
		[
			r"C:\Program Files (x86)\BYOND\bin",
			r"C:\Program Files\BYOND\bin",
			"/usr/local/byond/bin",
			"/opt/byond/bin",
		]
		.map(PathBuf::from),
	);
	candidates
		.into_iter()
		.find(|dir| dir.join(&compiler).exists())
		.context("BYOND not found; set BYOND_BIN to its bin directory")
}
