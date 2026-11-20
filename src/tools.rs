//! Tool catalogue advertised to the MCP client.

use serde_json::{json, Value};

const REFERENCES: &str = "Object references are JSON objects: {\"$ref\":\"[0x2000001]\"} (as returned by other tools), \
{\"$mob\":\"ckey\"}, {\"$client\":\"ckey\"}, {\"$global\":\"SSticker\"} (GLOB entry or global var), \
{\"$path\":\"/obj/item\"}, {\"$turf\":[x,y,z]}, {\"$world\":true}. They work anywhere inside args and values.";

fn tool(name: &str, description: String, properties: Value, required: &[&str]) -> Value {
	json!({
		"name": name,
		"description": description,
		"inputSchema": { "type": "object", "properties": properties, "required": required },
	})
}

pub fn definitions() -> Value {
	let wait = json!({ "type": "number", "description": "Seconds to wait before returning a still-running status." });
	let target = json!({ "description": "Object reference." });
	json!([
		tool(
			"build",
			"Compile the project's .dme into <name>.mcp.dmb with the test harness injected. The normal .dmb is not \
			 touched. Long builds return status \"running\"; call again to keep waiting."
				.into(),
			json!({
				"defines": { "type": "array", "items": { "type": "string" }, "description": "Extra -D defines, e.g. \"SKIP_LAVALAND\"." },
				"map": { "type": "string", "description": "tgstation-family: force this map (name of a _maps/*.json), e.g. a small debug map for fast startup." },
				"wait_secs": wait,
			}),
			&[],
		),
		tool(
			"server_start",
			"Run the harness build in DreamDaemon on a local port and wait until the world answers.".into(),
			json!({
				"port": { "type": "integer", "description": "Defaults to a free port." },
				"params": { "type": "object", "description": "Extra world.params entries." },
				"wait_secs": wait,
			}),
			&[],
		),
		tool("server_stop", "Stop the world and close every client this server opened.".into(), json!({}), &[]),
		tool(
			"server_status",
			"Process state plus the world's own report: world time, connected ckeys, game state and runtime count.".into(),
			json!({}),
			&[],
		),
		tool(
			"logs",
			"Read the world's logs. Without `file`, lists the round's log files and shows DreamDaemon's own output. \
			 `file` is a name from that list, or \"dd\" for DreamDaemon output."
				.into(),
			json!({
				"file": { "type": "string" },
				"tail": { "type": "integer", "description": "Keep only the last N lines (default 200)." },
				"grep": { "type": "string", "description": "Keep only lines containing this text (case-insensitive)." },
				"since_last": { "type": "boolean", "description": "Only what was written since the previous read of this file." },
			}),
			&[],
		),
		tool(
			"client_join",
			"Open a real Dream Seeker window connected to the world and wait for its client to log in. One machine \
			 has two identities, so at most two windows: \"guest\" (needs the BYOND pager closed) and \"account\" \
			 (the BYOND account saved in the pager, which is started briefly if it is not running). Further players \
			 have to be mobs without a client."
				.into(),
			json!({
				"identity": { "type": "string", "enum": ["guest", "account"], "description": "Default \"guest\"." },
				"wait_secs": wait,
			}),
			&[],
		),
		tool(
			"client_close",
			"Close a Dream Seeker window opened by client_join; all of them when `ckey` is omitted.".into(),
			json!({ "ckey": { "type": "string" } }),
			&[],
		),
		tool("clients", "List connected clients with their mobs.".into(), json!({}), &[]),
		tool(
			"client_command",
			"Make a client run a command exactly as if the player typed it: verbs with arguments (`say \"hello\"`), \
			 or skin commands like `.winset`."
				.into(),
			json!({ "ckey": { "type": "string" }, "command": { "type": "string" } }),
			&["ckey", "command"],
		),
		tool(
			"call_proc",
			format!(
				"Call a proc or verb and return its result. Without `target` it is a global proc. Sleeping procs keep \
				 running after `wait_secs`; fetch the result with job_result. Runtime errors come back as the error \
				 with its call stack. {REFERENCES}"
			),
			json!({
				"proc": { "type": "string", "description": "Proc name without path, e.g. \"attack_hand\"." },
				"target": target,
				"args": { "type": "array", "description": "Positional arguments.", "items": {} },
				"as_ckey": { "type": "string", "description": "Run with usr set to this client's mob." },
				"depth": { "type": "integer", "description": "How many list levels of the result to expand (default 2)." },
				"wait_secs": wait,
			}),
			&["proc"],
		),
		tool(
			"inspect",
			format!("Read variables of an object, or expand any value such as a global list. {REFERENCES}"),
			json!({
				"target": target,
				"vars": { "type": "array", "items": { "type": "string" }, "description": "Variable names; all of them when omitted." },
				"depth": { "type": "integer", "description": "How many list levels to expand (default 2)." },
			}),
			&["target"],
		),
		tool(
			"set_var",
			format!("Assign a variable directly, bypassing vv hooks. {REFERENCES}"),
			json!({ "target": target, "var": { "type": "string" }, "value": {} }),
			&["target", "var", "value"],
		),
		tool(
			"spawn",
			format!("Create an object: new path(loc, args...) for atoms, new path(args...) otherwise. {REFERENCES}"),
			json!({
				"path": { "type": "string" },
				"loc": { "description": "Reference to the location for atoms." },
				"args": { "type": "array", "items": {} },
			}),
			&["path"],
		),
		tool("delete", format!("Delete an object (qdel where the codebase has it). {REFERENCES}"), json!({ "target": target }), &["target"]),
		tool(
			"find",
			"Find existing objects of a type (subtypes included) anywhere in the world.".into(),
			json!({
				"path": { "type": "string" },
				"name": { "type": "string", "description": "Atoms only: name must contain this text." },
				"limit": { "type": "integer", "description": "Default 50." },
			}),
			&["path"],
		),
		tool(
			"eval_lua",
			"Run a Luau script in the world through dreamluau (tgstation-family codebases): full access to datums, \
			 procs and globals via `dm.global_procs`, `dm.global_vars`, `SS13`. Returns status, return values and \
			 print output. State persists between calls."
				.into(),
			json!({
				"script": { "type": "string" },
				"as_ckey": { "type": "string", "description": "Run with usr set to this client's mob." },
				"depth": { "type": "integer", "description": "How many list levels of the result to expand (default 4)." },
			}),
			&["script"],
		),
		tool(
			"job_result",
			"Result of a call that was still running when its tool returned.".into(),
			json!({ "job": { "type": "integer" }, "wait_secs": wait }),
			&["job"],
		),
		tool(
			"enter_game",
			"Get past the lobby in one step: join missing clients, start the round, give every connected player a \
			 body and put them next to each other. Players already in a living body are just moved. Returns the \
			 bodies. May return a running job while the round starts."
				.into(),
			json!({
				"players": { "type": "integer", "description": "How many Dream Seeker windows must be connected: 0 (use whoever is connected), 1 or 2. Default 2." },
				"x": { "type": "integer", "description": "Where to stand; defaults to the map's late-join arrival point." },
				"y": { "type": "integer" },
				"z": { "type": "integer" },
				"path": { "type": "string", "description": "Mob type for new bodies; defaults to a human." },
				"use_prefs": { "type": "boolean", "description": "Apply each player's saved character to a new human body (default true)." },
				"outfit": { "type": "string", "description": "Outfit type path to equip on new bodies." },
				"fresh": { "type": "boolean", "description": "Give new bodies even to players who already have one." },
				"own_character": { "type": "boolean", "description": "Register each new body as the player's own spawned character, as a lobby join would. Needed for code that saves progress back to the character mid-round; such saves then really modify the player's save file." },
				"bodies": { "type": "integer", "description": "Extra uncontrolled bodies of the same type placed alongside, for tests that need more participants than two windows." },
				"wait_secs": wait,
			}),
			&[],
		),
		tool(
			"ui_list",
			"What a player has open and waiting for an answer: tgui windows (with the data they display, e.g. an \
			 alert's buttons) and radial menus (with their choices)."
				.into(),
			json!({
				"ckey": { "type": "string" },
				"depth": { "type": "integer", "description": "How many list levels of window data to expand (default 5)." },
			}),
			&["ckey"],
		),
		tool(
			"ui_act",
			"Answer a player's open interface through the same server handler their click reaches. Radial menu: \
			 pass `choice`. tgui window: pass `action` and `params` as the interface would send them, e.g. \
			 alert -> action \"choose\", params {\"choice\": \"Yes\"}; list input -> action \"submit\", params \
			 {\"entry\": \"...\"}. `ui` (from ui_list) is only needed when several are open."
				.into(),
			json!({
				"ckey": { "type": "string" },
				"ui": { "description": "The `ui` reference from ui_list." },
				"choice": { "type": "string", "description": "Radial menu choice label." },
				"action": { "type": "string", "description": "tgui action name." },
				"params": { "type": "object", "description": "tgui action parameters." },
			}),
			&["ckey"],
		),
		tool(
			"screenshot",
			"Capture a Dream Seeker window opened by client_join as PNG. Picks the largest window unless `window` \
			 names one by title (tgui panels are separate windows)."
				.into(),
			json!({
				"ckey": { "type": "string", "description": "Defaults to the first client." },
				"window": { "type": "string", "description": "Part of the window title." },
			}),
			&[],
		),
	])
}
