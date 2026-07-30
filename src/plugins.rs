use std::collections::BTreeMap;
use std::io::BufReader;
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{Receiver, RecvTimeoutError, sync_channel};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use false_agent_protocol::{
    RepositoryRef,
    framing::{read_frame, write_frame},
    plugin::{
        CapabilityId, HostInfo, InitializeParams, InitializeResult, PluginId, PluginInfo,
        PluginKind, ProtocolRange, RepositoryIdentity, RequestId, RpcRequest, RpcResponse, WORK_V1,
        WORKER_V1,
    },
};
use serde::Deserialize;
use serde_json::Value;

#[derive(Debug, Clone, Deserialize)]
pub struct PluginDefinition {
    pub kind: PluginKind,
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub required: bool,
    #[serde(default = "default_timeout")]
    pub timeout_ms: u64,
}

#[derive(Debug, Default, Deserialize)]
struct PluginFile {
    #[serde(default)]
    plugins: BTreeMap<String, PluginDefinition>,
    #[serde(default = "default_auto_discovery")]
    automatic_path_discovery: bool,
}

fn default_timeout() -> u64 {
    5_000
}

fn default_auto_discovery() -> bool {
    true
}

pub fn definitions() -> Result<BTreeMap<String, PluginDefinition>> {
    let Some(path) = user_config_path() else {
        return Ok(BTreeMap::new());
    };
    if !path.is_file() {
        return Ok(BTreeMap::new());
    }
    let source = std::fs::read_to_string(&path)
        .with_context(|| format!("failed to read {}", path.display()))?;
    Ok(toml::from_str::<PluginFile>(&source)
        .with_context(|| format!("failed to parse {}", path.display()))?
        .plugins)
}

pub fn discover(id: &str, kind: PluginKind) -> Result<Option<PluginDefinition>> {
    let path = user_config_path();
    let file = match path.as_ref().filter(|path| path.is_file()) {
        Some(path) => {
            let source = std::fs::read_to_string(path)
                .with_context(|| format!("failed to read {}", path.display()))?;
            toml::from_str::<PluginFile>(&source)
                .with_context(|| format!("failed to parse {}", path.display()))?
        }
        None => PluginFile {
            plugins: BTreeMap::new(),
            automatic_path_discovery: true,
        },
    };
    if let Some(definition) = file.plugins.get(id) {
        if definition.kind != kind {
            bail!("plugin `{id}` is configured as {:?}", definition.kind);
        }
        return Ok(Some(definition.clone()));
    }
    if file.automatic_path_discovery
        && matches!(
            (id, kind),
            ("teko", PluginKind::Work) | ("kisko", PluginKind::Worker)
        )
        && which::which(id).is_ok()
    {
        return Ok(Some(PluginDefinition {
            kind,
            command: id.into(),
            args: vec!["plugin".into(), "serve".into(), "--stdio".into()],
            required: false,
            timeout_ms: default_timeout(),
        }));
    }
    Ok(None)
}

pub fn user_config_path() -> Option<PathBuf> {
    std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".config")))
        .map(|root| root.join("toimija/plugins.toml"))
}

pub struct PluginSession {
    child: Child,
    input: Option<ChildStdin>,
    responses: Receiver<Result<RpcResponse, String>>,
    reader: Option<JoinHandle<()>>,
    next_id: u64,
    timeout: Duration,
    pub metadata: PluginInfo,
}

impl PluginSession {
    pub fn spawn(
        id: &str,
        definition: &PluginDefinition,
        repo_root: &Path,
        repository: RepositoryRef,
    ) -> Result<Self> {
        let mut command = Command::new(&definition.command);
        command
            .args(&definition.args)
            .current_dir(repo_root)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit());
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            command.process_group(0);
        }
        let mut child = command
            .spawn()
            .with_context(|| format!("failed to spawn plugin `{id}`"))?;
        let input = child.stdin.take().context("plugin stdin was not piped")?;
        let output = child.stdout.take().context("plugin stdout was not piped")?;
        let (sender, responses) = sync_channel(8);
        let reader = std::thread::spawn(move || {
            let mut output = BufReader::new(output);
            loop {
                let response = match read_frame(&mut output) {
                    Ok(Some(frame)) => serde_json::from_slice(&frame)
                        .map_err(|error| format!("invalid plugin response: {error}")),
                    Ok(None) => break,
                    Err(error) => {
                        let _ = sender.send(Err(error.to_string()));
                        break;
                    }
                };
                if sender.send(response).is_err() {
                    break;
                }
            }
        });
        let unavailable = PluginInfo {
            id: PluginId::new("uninitialized").map_err(anyhow::Error::from)?,
            name: "uninitialized".into(),
            version: "0".into(),
            kind: definition.kind,
        };
        let mut session = Self {
            child,
            input: Some(input),
            responses,
            reader: Some(reader),
            next_id: 1,
            timeout: Duration::from_millis(definition.timeout_ms),
            metadata: unavailable,
        };
        let capability = match definition.kind {
            PluginKind::Work => WORK_V1,
            PluginKind::Worker => WORKER_V1,
        };
        let result: InitializeResult = session.call(
            false_agent_protocol::plugin::INITIALIZE_METHOD,
            InitializeParams {
                host: HostInfo {
                    name: "toimija".into(),
                    version: env!("CARGO_PKG_VERSION").into(),
                },
                protocol: ProtocolRange::v1(),
                requested_capabilities: vec![
                    CapabilityId::new(capability).map_err(anyhow::Error::from)?,
                ],
                repository: Some(RepositoryIdentity {
                    id: repository,
                    root_hint: repo_root.display().to_string(),
                }),
            },
        )?;
        result
            .validate(&[CapabilityId::new(capability).map_err(anyhow::Error::from)?])
            .map_err(anyhow::Error::from)?;
        if result.plugin.id.as_str() != id || result.plugin.kind != definition.kind {
            bail!("plugin identity mismatch for `{id}`");
        }
        session.metadata = result.plugin;
        Ok(session)
    }

    pub fn call<P: serde::Serialize, R: serde::de::DeserializeOwned>(
        &mut self,
        method: &str,
        params: P,
    ) -> Result<R> {
        let id = RequestId::new(self.next_id).map_err(anyhow::Error::from)?;
        self.next_id = self
            .next_id
            .checked_add(1)
            .context("plugin request ID exhausted")?;
        let request = RpcRequest::new(id, method, serde_json::to_value(params)?);
        let bytes = serde_json::to_vec(&request)?;
        write_frame(
            self.input.as_mut().context("plugin stdin is closed")?,
            &bytes,
        )
        .map_err(anyhow::Error::from)?;
        let deadline = Instant::now() + self.timeout;
        let response = loop {
            let response = match self
                .responses
                .recv_timeout(deadline.saturating_duration_since(Instant::now()))
            {
                Ok(response) => response.map_err(anyhow::Error::msg)?,
                Err(RecvTimeoutError::Timeout) => bail!("plugin request `{method}` timed out"),
                Err(RecvTimeoutError::Disconnected) => bail!("plugin crashed during `{method}`"),
            };
            response.validate().map_err(anyhow::Error::from)?;
            if response.id == id {
                break response;
            }
            if response.id.get() > id.get() {
                bail!(
                    "plugin returned unknown response ID {}; expected {}",
                    response.id.get(),
                    id.get()
                );
            }
        };
        if let Some(error) = response.error {
            bail!("{:?}: {}", error.code, error.message);
        }
        serde_json::from_value(
            response
                .result
                .context("validated response omitted its result")?,
        )
        .context("plugin returned an invalid result")
    }

    fn shutdown(&mut self) {
        if self.input.is_some() {
            let _: Result<Value> = self.call(
                false_agent_protocol::plugin::SHUTDOWN_METHOD,
                serde_json::json!({}),
            );
        }
        self.input.take();
        let deadline = Instant::now() + Duration::from_millis(1_000);
        while Instant::now() < deadline {
            match self.child.try_wait() {
                Ok(Some(_)) => {
                    kill_process_group(&mut self.child);
                    return;
                }
                Ok(None) => std::thread::sleep(Duration::from_millis(10)),
                Err(_) => break,
            }
        }
        kill_process_group(&mut self.child);
        let _ = self.child.wait();
    }
}

fn kill_process_group(child: &mut Child) {
    #[cfg(unix)]
    if let Ok(pid) = i32::try_from(child.id()) {
        // SAFETY: `process_group(0)` made the child PID its process-group ID.
        unsafe {
            libc::kill(-pid, libc::SIGKILL);
        }
    }
    let _ = child.kill();
}

impl Drop for PluginSession {
    fn drop(&mut self) {
        self.shutdown();
        if let Some(reader) = self.reader.take() {
            let _ = reader.join();
        }
    }
}

pub fn print_list() -> Result<()> {
    let definitions = definitions()?;
    if definitions.is_empty() {
        println!("no configured plugins");
    } else {
        for (id, plugin) in definitions {
            println!("{id}\t{:?}\t{}", plugin.kind, plugin.command);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(value: serde_json::Value) -> String {
        let body = serde_json::to_string(&value).unwrap();
        format!("Content-Length: {}\r\n\r\n{body}", body.len())
    }

    fn definition(script: String, timeout_ms: u64) -> PluginDefinition {
        PluginDefinition {
            kind: PluginKind::Work,
            command: "sh".into(),
            args: vec!["-c".into(), script],
            required: false,
            timeout_ms,
        }
    }

    #[test]
    fn session_initializes_and_rejects_unknown_response_ids() {
        let initialize = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "result": {
                "plugin": {"id": "teko", "name": "fake", "version": "1", "kind": "work"},
                "selected_protocol": {"major": 1, "minor": 0},
                "capabilities": [{"id": "work.v1", "methods": ["work.current"]}],
                "limits": false_agent_protocol::plugin::PluginLimits::default()
            }
        });
        let shutdown = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 2,
            "result": {}
        });
        let frames = format!("{}{}", frame(initialize), frame(shutdown));
        let script = format!("printf %s '{frames}'; sleep 1");
        let repository: RepositoryRef = "repo_fixture".parse().unwrap();
        let session = PluginSession::spawn(
            "teko",
            &definition(script, 1_000),
            Path::new("."),
            repository.clone(),
        )
        .unwrap();
        drop(session);

        let wrong = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 9,
            "result": {
                "plugin": {"id": "teko", "name": "fake", "version": "1", "kind": "work"},
                "selected_protocol": {"major": 1, "minor": 0},
                "capabilities": [{"id": "work.v1", "methods": ["work.current"]}],
                "limits": false_agent_protocol::plugin::PluginLimits::default()
            }
        });
        let script = format!("printf %s '{}'; sleep 1", frame(wrong));
        let error = PluginSession::spawn(
            "teko",
            &definition(script, 1_000),
            Path::new("."),
            repository,
        )
        .err()
        .unwrap()
        .to_string();
        assert!(error.contains("unknown response ID"), "{error}");
    }

    #[test]
    fn session_timeout_is_bounded() {
        let started = Instant::now();
        let error = PluginSession::spawn(
            "teko",
            &definition("sleep 5".into(), 10),
            Path::new("."),
            "repo_fixture".parse().unwrap(),
        )
        .err()
        .unwrap()
        .to_string();
        assert!(error.contains("timed out"), "{error}");
        assert!(started.elapsed() < Duration::from_secs(2));
    }

    #[test]
    fn session_discards_a_late_timed_out_response() {
        let responses = [
            serde_json::json!({
                "jsonrpc": "2.0",
                "id": 1,
                "result": {
                    "plugin": {"id": "teko", "name": "fake", "version": "1", "kind": "work"},
                    "selected_protocol": {"major": 1, "minor": 0},
                    "capabilities": [{"id": "work.v1", "methods": ["work.current"]}],
                    "limits": false_agent_protocol::plugin::PluginLimits::default()
                }
            }),
            serde_json::json!({"jsonrpc": "2.0", "id": 2, "result": "late"}),
            serde_json::json!({"jsonrpc": "2.0", "id": 3, "result": "current"}),
            serde_json::json!({"jsonrpc": "2.0", "id": 4, "result": {}}),
        ];
        let late_frames = format!(
            "{}{}{}",
            frame(responses[1].clone()),
            frame(responses[2].clone()),
            frame(responses[3].clone())
        );
        let script = format!(
            "printf %s '{}'; sleep 0.05; printf %s '{late_frames}'; sleep 1",
            frame(responses[0].clone())
        );
        let mut session = PluginSession::spawn(
            "teko",
            &definition(script, 10),
            Path::new("."),
            "repo_fixture".parse().unwrap(),
        )
        .unwrap();

        assert!(
            session
                .call::<_, Value>("slow", serde_json::json!({}))
                .is_err()
        );
        std::thread::sleep(Duration::from_millis(60));
        let response: String = session.call("next", serde_json::json!({})).unwrap();

        assert_eq!(response, "current");
    }
}
