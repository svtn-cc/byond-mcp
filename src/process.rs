//! Child processes whose output is collected while they run.

use std::io::Read;
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};

pub struct Captured {
	child: Child,
	output: Arc<Mutex<Vec<u8>>>,
	pub started: Instant,
}

impl Captured {
	pub fn spawn(mut command: Command) -> Result<Self> {
		hide_console(&mut command);
		let mut child = command
			.stdin(Stdio::null())
			.stdout(Stdio::piped())
			.stderr(Stdio::piped())
			.spawn()
			.with_context(|| format!("cannot start {:?}", command.get_program()))?;
		let output = Arc::new(Mutex::new(Vec::new()));
		pump(child.stdout.take(), &output);
		pump(child.stderr.take(), &output);
		Ok(Self { child, output, started: Instant::now() })
	}

	pub fn pid(&self) -> u32 {
		self.child.id()
	}

	pub fn output(&self) -> String {
		String::from_utf8_lossy(&self.output.lock().unwrap()).into_owned()
	}

	/// Exit code once the process has ended; `-1` when it was killed without one.
	pub fn exit_code(&mut self) -> Option<i32> {
		self.child.try_wait().ok().flatten().map(|status| status.code().unwrap_or(-1))
	}

	pub fn wait_for(&mut self, limit: Duration) -> Option<i32> {
		let deadline = Instant::now() + limit;
		loop {
			if let Some(code) = self.exit_code() {
				return Some(code);
			}
			if Instant::now() >= deadline {
				return None;
			}
			thread::sleep(Duration::from_millis(100));
		}
	}

	pub fn kill(&mut self) {
		let _ = self.child.kill();
		let _ = self.child.wait();
	}
}

fn pump(stream: Option<impl Read + Send + 'static>, sink: &Arc<Mutex<Vec<u8>>>) {
	let Some(mut stream) = stream else { return };
	let sink = Arc::clone(sink);
	thread::spawn(move || {
		let mut chunk = [0u8; 4096];
		while let Ok(read) = stream.read(&mut chunk) {
			if read == 0 {
				break;
			}
			sink.lock().unwrap().extend_from_slice(&chunk[..read]);
		}
	});
}

/// Console children would otherwise pop a window, since the MCP server itself has no console.
#[cfg(windows)]
pub fn hide_console(command: &mut Command) {
	use std::os::windows::process::CommandExt;
	const CREATE_NO_WINDOW: u32 = 0x0800_0000;
	command.creation_flags(CREATE_NO_WINDOW);
}

#[cfg(not(windows))]
pub fn hide_console(_command: &mut Command) {}
