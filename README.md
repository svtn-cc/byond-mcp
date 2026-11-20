# byond-mcp

An [MCP](https://modelcontextprotocol.io/) server that lets an AI agent build, run and drive a
[BYOND](https://www.byond.com/) world: compile the project, start Dream Daemon, open real Dream Seeker
clients, call procs, inspect and change state, answer tgui windows, read logs and take screenshots.

It is meant for testing game code you are writing. The agent changes DM code, rebuilds, joins the game
and checks that the feature actually works.

Works with any BYOND project. Codebases from the tgstation family get extra features (round control,
Luau scripting, tgui, per-round logs).

## How it works

A running BYOND world only accepts outside input through `world/Topic()`. A normal build has no handler
there that could call arbitrary procs, so byond-mcp makes its own build:

1. `build` copies your `.dme`, appends one include with a small DM test harness, and compiles it as
   `<name>.mcp.dmb`. The temporary source files are removed afterwards.
2. `server_start` runs that build in Dream Daemon on a local port with a random token in `world.params`.
3. Every tool that reaches into the game sends a token-protected topic packet to the harness.

Your regular `<name>.dmb` and your sources are never modified. The harness exists only in the
`.mcp.dmb` build and does nothing unless the world was started with a token.

> **Security.** The harness executes any proc for whoever holds the token. Never deploy a `.mcp.dmb`
> build to a public server.

## Requirements

- BYOND 516 (tested with 516.1669). The harness uses `try`/`catch`, `isnan`, `ref` strings and
  `world.params`.
- Windows for clients and screenshots. The server itself should build elsewhere, but only Windows has
  been tested, and `client_join`, `enter_game` (when it has to open clients) and `screenshot` are
  Windows-only.
- A recent stable Rust toolchain to build (developed with 1.90).

## Installation

```sh
git clone <this repository>
cd byond-mcp
cargo build --release
```

The binary is `target/release/byond-mcp.exe`.

## Configuration

The server talks JSON-RPC over stdio. Point your MCP client at the binary and pass the project
directory, which must contain exactly one `.dme` (files named `*.test.dme` are ignored):

```json
{
  "mcpServers": {
    "byond": {
      "command": "C:/path/to/byond-mcp/target/release/byond-mcp.exe",
      "args": ["--project", "C:/path/to/your/project"]
    }
  }
}
```

| Setting | Meaning |
|---|---|
| `--project <dir>` | Project directory. |
| `BYOND_MCP_PROJECT` | Project directory, used when `--project` is absent. Falls back to the working directory. |
| `BYOND_BIN` | BYOND `bin` directory. Defaults to the standard install locations. |

Since the server is tied to one project, a project-level config is the natural place for it, for
example `.claude/mcp.json`, `.cursor/mcp.json` or `.omp/mcp.json`.

## Tools

### Build and server

| Tool | Description |
|---|---|
| `build` | Compile `<name>.mcp.dmb` with the harness. Accepts extra `defines` and, for tgstation-family codebases, a forced `map`. Long builds return `running`; call again to keep waiting. |
| `server_start` | Run the harness build in Dream Daemon and wait until the world answers. |
| `server_stop` | Stop the world and close every client this server opened. |
| `server_status` | Process state plus the world's own report: world time, connected ckeys, init and round state, runtime count. |
| `logs` | Read Dream Daemon output or a log file of the current round, with `tail`, `grep` and `since_last`. |

### Clients

| Tool | Description |
|---|---|
| `client_join` | Open a real Dream Seeker window and wait for it to log in, as `guest` or `account`. |
| `client_close` | Close one window by ckey, or all of them. |
| `clients` | List connected clients and their mobs. |
| `client_command` | Make a client run a command as if the player typed it. |
| `enter_game` | Skip the lobby in one call: join missing clients, start the round, give each player a body and place everyone side by side. |
| `screenshot` | Capture a client window as PNG. |

### Inside the game

| Tool | Description |
|---|---|
| `call_proc` | Call a global proc, or a proc or verb on an object, optionally with `usr` set to a player. |
| `inspect` | Read variables of an object, or expand any value such as a global list. |
| `set_var` | Assign a variable directly. |
| `spawn` | Create an object. |
| `delete` | Delete an object (`qdel` where the codebase has it). |
| `find` | Find existing objects of a type. |
| `eval_lua` | Run a Luau script through dreamluau. |
| `ui_list` | Show a player's open tgui windows, with their data, and radial menus, with their choices. |
| `ui_act` | Answer a tgui window or radial menu through the handler a real click reaches. |
| `job_result` | Fetch the result of a call that was still running when its tool returned. |

### Object references

Objects are passed as small JSON objects, and tools return them in the same form, so a result can be
fed straight into the next call:

| Reference | Resolves to |
|---|---|
| `{"$ref": "[0x2000001]"}` | The object with that BYOND ref. |
| `{"$mob": "ckey"}` | That player's mob. |
| `{"$client": "ckey"}` | That player's client. |
| `{"$global": "SSticker"}` | A `GLOB` entry or a global variable. |
| `{"$path": "/obj/item"}` | A type path. |
| `{"$turf": [x, y, z]}` | A turf. |
| `{"$world": true}` | `world`. |

References work anywhere inside `args` and values, including nested lists.

### Sleeping procs and errors

Every call runs detached from `world/Topic()`. If the proc sleeps longer than `wait_secs`, the tool
returns `{"status": "running", "job": N}` and the result is fetched later with `job_result`.

A runtime error in the called code is caught and returned as the tool's error, with the call stack.

## Example session

```text
build         {"map": "runtimetown"}
server_start  {}
server_status {}                          -> wait for "initialized": 1
enter_game    {}                          -> two players in bodies, next to each other
call_proc     {"target": {"$mob": "guest123"}, "proc": "adjust_brute_loss", "args": [15]}
inspect       {"target": {"$mob": "guest123"}, "vars": ["health", "stat"]}
ui_list       {"ckey": "guest123"}
ui_act        {"ckey": "guest123", "action": "choose", "params": {"choice": "Accept"}}
screenshot    {"ckey": "guest123"}
logs          {"file": "runtime.log", "grep": "runtime error"}
server_stop   {}
```

## Clients and identities

BYOND gives one machine two identities:

- **guest**: the key is derived from the hardware id, so every guest on the machine is the same
  `Guest-NNNN`. Dream Seeker logs in as a guest when the BYOND pager is not running.
- **account**: the account saved in the BYOND pager. Dream Seeker takes its key from a running pager.
  A client keeps its key after the pager exits.

So at most two real windows can be connected at once. For an `account` join the server starts the pager
for a few seconds if it is not already running, and closes it again if it started it. A `guest` join
fails with an explanation when your own pager is open.

For more participants use `enter_game` with `bodies`, which adds uncontrolled bodies, or log the pager
into another BYOND account and join with `identity: "account"` again.

### Bodies from `enter_game` versus a lobby join

`enter_game` creates bodies directly instead of going through the lobby, so the game does not consider
them the players' own spawned characters. Code that writes progress back to the character's save
mid-round usually checks for that and refuses.

Pass `own_character: true` to record what a lobby join records (the original character slot on the
mind and the slot the client joined as). Those saves then succeed, which means the test **really
modifies the player's save file**. It is off by default for that reason.

## tgstation-family features

These rely on tgstation internals and are unavailable elsewhere:

| Feature | Uses |
|---|---|
| `initialized` and `game_state` in `server_status` | `Master`, `SSticker` |
| Round start, saved character and `outfit` in `enter_game` | `SSticker`, preferences, `PossessByPlayer` |
| `eval_lua` | dreamluau and `/datum/lua_state` |
| `ui_list`, `ui_act` | tgui and `/datum/radial_menu` |
| Per-round log files in `logs` | the `log-directory` world parameter |
| `map` in `build` | the `FORCE_MAP` define |

A full station map can take minutes to initialize. A small debug map plus defines such as
`SKIP_LAVALAND`, `SKIP_SPACE_LEVELS` and `MINIMAL_CENTCOM` cuts that to seconds, if your codebase has
them.

## Limitations

- `ui_act` drives the server-side handler. It tests game logic, not the window's layout or JavaScript.
- `client_command` passes arguments only to verbs that declare them. Codebases whose verbs prompt for
  input instead need `call_proc` on the verb's body.
- tgui windows have an empty window title, so `screenshot` cannot pick one by name.
- `find` walks every atom in the world for atom types, which is slow on large maps.
- Lists are cut at 300 entries and expanded two levels deep by default; raise `depth` where a tool
  offers it.
- After changing DM code you have to call `build` again. Your normal build does not update the
  `.mcp.dmb`.

## Files it creates in your project

| Path | When |
|---|---|
| `<name>.mcp.dmb`, `<name>.mcp.rsc` and their lock and cache files | Output of `build`. |
| `<name>.mcp.dme`, `<name>.mcp_harness.dm` | Only while the compiler runs. |
| `data/mcp_harness/` | Requests and replies too large for one topic packet; removed after use. |

Add `*.mcp.dme` and `*.mcp_harness.dm` to `.gitignore` in case a build is interrupted.

## Development

```text
dm/mcp_harness.dm     DM harness, embedded into the binary at compile time
src/main.rs           JSON-RPC loop
src/tools.rs          tool catalogue
src/app.rs            tool implementations
src/topic.rs          world/Topic packet client
src/process.rs        child processes with captured output
src/screenshot.rs     window capture (Windows)
tests/world/          minimal world for trying the server without a large codebase
```

There is no automated test suite. `tests/world/fixture.dme` compiles in under a second and exercises
everything that is not tgstation-specific: point `--project` at `tests/world` and drive the tools from
any MCP client.

Changes to `dm/mcp_harness.dm` take effect after rebuilding the server and then calling `build` again.
