use anyhow::{anyhow, bail, Result};
use buttplug::client::{ButtplugClient, ButtplugClientDevice, ButtplugClientEvent, VibrateCommand};
use buttplug::core::connector::{ButtplugRemoteClientConnector, ButtplugWebsocketClientTransport};
use buttplug::core::message::serializer::ButtplugClientJSONSerializer;
use buttplug::core::message::ActuatorType;
use clap::Parser;
use futures::{select, FutureExt, StreamExt};
use nix;
use std::ffi::OsString;
use std::io::Write;
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;
use sysinfo::{Pid, ProcessExt, ProcessRefreshKind, RefreshKind, System, SystemExt};
use tempfile;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;
use tokio::sync::oneshot::error::TryRecvError;
use tokio::sync::{oneshot, watch, Mutex};
use tokio::time::{sleep, timeout};
use tokio::{signal, spawn};
use tracing::{error, info, warn};

#[derive(Parser, Debug)]
#[command(author, version, about, long_about)]
/// StarPlug tracks your APM and sends it to your vibrator.
///
/// Launch StarPlug after starting Intiface Central's server and before starting StarCraft itself.
///
/// StarPlug on macOS requires `lldb`; you can install it with the Xcode command-line tools by running `xcode-select --install`.
struct Args {
    /// Intiface websocket URL to connect to.
    #[arg(long, default_value = "ws://localhost:12345")]
    server: String,

    /// Don't vibrate below this APM.
    #[arg(long, default_value_t = 40)]
    min_apm: i32,

    /// Max vibration at this APM.
    #[arg(long, default_value_t = 100)]
    max_apm: i32,

    /// Show lldb errors (only useful for debugging, most aren't signficant).
    #[arg(long, default_value_t = false)]
    show_lldb_errors: bool,
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt::init();

    let args = Args::parse();
    if args.max_apm <= args.min_apm {
        bail!("Max APM must be strictly greater than min APM!");
    }
    if args.min_apm < 0 {
        bail!("APM values cannot be negative!");
    }

    check_prereqs().await?;

    info!("Type Ctrl-C to quit StarPlug.");

    info!("Connecting to Intiface…");
    let client = Arc::new(Mutex::new(ButtplugClient::new("StarPlug")));
    let server = args.server.clone();
    connect_to_buttplug(server.clone(), client.clone()).await?;
    spawn(stay_connected_to_buttplug(server.clone(), client.clone()));
    info!("Connected to Intiface.");

    let running_lldb: Arc<Mutex<Option<ChildShutdown>>> = Arc::new(Mutex::new(None));

    loop {
        select! {
            signal_result = signal::ctrl_c().fuse() => {
                if signal_result.is_err() {
                    // This probably won't happen unless we can't install a Ctrl-C handler.
                    return signal_result.map_err(|e| anyhow!(e));
                }
                if let Some(lldb) = running_lldb.lock().await.take() {
                    info!("Waiting for lldb to terminate…");
                    lldb.terminate().await?;
                    info!("lldb terminated.");
                }
                return Ok(());
            }
            sync_result = sync_apm_to_vibrators(&args, client.clone(), running_lldb.clone()).fuse() => {
                if sync_result.is_err() {
                    return sync_result;
                }
                info!("Lost connection to StarCraft.");
                info!("Waiting for StarCraft to be relaunched…");
            }
        }
    }
}

async fn check_prereqs() -> Result<()> {
    let exit_status = Command::new("lldb")
        .arg("--version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| anyhow!(e).context("Couldn't run `lldb --version`. Make sure it's installed by running `xcode-select --install`."))?
        .wait()
        .await?;
    if !exit_status.success() {
        bail!("`lldb --version` failed with status {exit_status}. Make sure it's installed by running `xcode-select --install`.");
    }
    Ok(())
}

/// Wait this long between attempts to connect to Intiface.
const BUTTPLUG_WAIT: Duration = Duration::from_secs(5);

/// Connect to an Intiface server.
async fn connect_to_buttplug(server: String, client: Arc<Mutex<ButtplugClient>>) -> Result<()> {
    while let Err(e) = client
        .lock()
        .await
        .connect(ButtplugRemoteClientConnector::<
            ButtplugWebsocketClientTransport,
            ButtplugClientJSONSerializer,
        >::new(
            ButtplugWebsocketClientTransport::new_insecure_connector(&server),
        ))
        .await
    {
        warn!("Couldn't connect to Intiface: {e}");
        info!("Please make sure the Intiface server is running and listening at {server}. Waiting {wait:?} and trying again…", wait = BUTTPLUG_WAIT);
        sleep(BUTTPLUG_WAIT).await;
    }
    info!("Connected to Intiface.");
    client
        .lock()
        .await
        .start_scanning()
        .await
        .map_err(|e| anyhow!(e).context("Couldn't start scanning for vibrators."))
}

async fn stay_connected_to_buttplug(server: String, client: Arc<Mutex<ButtplugClient>>) {
    let mut client_events = client.lock().await.event_stream();
    while let Some(event) = client_events.next().await {
        match event {
            ButtplugClientEvent::ServerDisconnect => {
                warn!("Disconnected from Intiface. Vibration disabled. Attempting to reconnect…");
                if let Err(e) = connect_to_buttplug(server.clone(), client.clone()).await {
                    error!("Error while reconnecting to Intiface: {e}");
                }
                info!("Reconnected to Intiface. Vibration enabled.");
            }
            ButtplugClientEvent::Error(e) => {
                error!("Intiface client error: {e}");
            }
            ButtplugClientEvent::PingTimeout => {
                error!("Intiface client ping timeout!");
            }
            _ => {}
        }
    }
}

trait ButtplugClientDeviceExt {
    fn is_vibrator(&self) -> bool;
}

impl ButtplugClientDeviceExt for ButtplugClientDevice {
    fn is_vibrator(&self) -> bool {
        if let Some(scalar_cmds) = self.message_attributes().scalar_cmd() {
            return scalar_cmds
                .iter()
                .find(|scalar_cmd| *scalar_cmd.actuator_type() == ActuatorType::Vibrate)
                .is_some();
        }
        false
    }
}

/// When it's been this long since the last APM change, stop all vibrators.
const GAME_RUNNING_WAIT: Duration = Duration::from_secs(3);

/// Monitor StarCraft.
/// Send vibration commands when APM changes.
/// Stop all vibrators if we don't get an APM change for a while.
async fn sync_apm_to_vibrators(
    args: &Args,
    client: Arc<Mutex<ButtplugClient>>,
    running_lldb: Arc<Mutex<Option<ChildShutdown>>>,
) -> Result<()> {
    info!("Starting lldb…");
    let mut apm_rx = connect_to_starcraft(args.show_lldb_errors, running_lldb).await?;
    info!("lldb started.");

    let mut game_running = false;
    loop {
        match timeout(GAME_RUNNING_WAIT, apm_rx.changed()).await {
            Ok(Ok(())) => {
                if !game_running {
                    info!("Connected to StarCraft: received first APM change.");
                    game_running = true;
                }
                let apm = *apm_rx.borrow_and_update();
                apm_changed(args, apm, client.clone()).await;
            }
            Ok(Err(e)) => {
                error!("APM channel closed: {e}");
                stop_all_vibrators(client.clone()).await;
                return Ok(());
            }
            Err(_) => {
                if game_running {
                    info!(
                        "APM hasn't changed in a while. \
                        The current game may have finished or StarCraft may be paused."
                    );
                    game_running = false;
                    stop_all_vibrators(client.clone()).await;
                }
            }
        }
    }
}

/// Python script that we ask `lldb` to run.
/// Writes status lines like `APM: 69`.
const STARPLUG_PY: &[u8] = include_bytes!("starplug.py");

/// Launch `lldb` with our instrumentation script and start tracking APM.
/// May need to wait for StarCraft to be started.
async fn connect_to_starcraft(
    show_lldb_errors: bool,
    running_lldb: Arc<Mutex<Option<ChildShutdown>>>,
) -> Result<watch::Receiver<i32>> {
    // Write our internal copy of the lldb script to a temp file.
    let mut starplug_py = tempfile::Builder::new()
        .prefix("starplug_")
        .suffix(".py")
        .tempfile()?;
    starplug_py.write_all(STARPLUG_PY)?;
    let starplug_py_path = starplug_py.path();

    // Build an lldb command to run the script.
    let mut script_arg = OsString::new();
    script_arg.push("command script import '");
    script_arg.push(starplug_py_path.as_os_str());
    script_arg.push("'");

    // Start lldb with that command.
    // lldb dumps a lot of symbol-related errors when loading the StarCraft binary,
    // but none of them matter.
    let mut lldb_cmd = Command::new("lldb");

    lldb_cmd
        .args(["--batch", "--source-quietly", "--one-line"])
        .arg(script_arg)
        .stdout(Stdio::piped())
        .stderr(if show_lldb_errors {
            Stdio::inherit()
        } else {
            Stdio::null()
        });

    if let Some(pid) = find_starcraft_pid() {
        info!("StarCraft is already running: PID {pid}");
        lldb_cmd.env("STARCRAFT_PID", pid.to_string());
    } else {
        info!("StarCraft is not running yet.");
    }

    let mut lldb = lldb_cmd.spawn()?;

    let lldb_stdout = lldb
        .stdout
        .take()
        .ok_or(anyhow!("Couldn't get lldb's stdout!"))?;
    let mut lldb_reader = BufReader::new(lldb_stdout).lines();

    let (apm_tx, apm_rx) = watch::channel(0i32);

    // Spawn a task to watch for APM info from lldb.
    let _ = tokio::spawn(async move {
        let mut prev_apm = 0i32;
        while let Ok(Some(line)) = lldb_reader.next_line().await {
            if let Some(apm_str_ws) = line.strip_prefix("APM:") {
                if let Ok(apm) = apm_str_ws.trim().parse::<i32>() {
                    if apm == prev_apm {
                        continue;
                    }
                    prev_apm = apm;
                    if let Err(e) = apm_tx.send(apm) {
                        error!("Couldn't send APM through watch channel: {e:?}");
                        break;
                    }
                }
            }
        }
        info!("lldb reader task finished.");
    });

    let (finished_tx, finished_rx) = oneshot::channel::<()>();
    let pid = lldb.id().ok_or(anyhow!("Couldn't get lldb PID!"))? as i32;
    *running_lldb.lock().await = Some(ChildShutdown { pid, finished_rx });

    // Spawn a task to wait for the lldb process so that it can make progress.
    let _ = tokio::spawn(async move {
        // Hold onto the temporary file until lldb finishes.
        let _starplug_py = starplug_py;

        match lldb.wait().await {
            Ok(status) => {
                if status.success() {
                    info!("lldb exited normally.");
                } else {
                    if let Some(code) = status.code() {
                        error!("lldb exited with code {code}!");
                    } else {
                        error!("lldb exited due to a signal!");
                    }
                }
            }
            Err(e) => error!("Couldn't wait for lldb to exit: {e:?}"),
        }
        let _ = finished_tx.send(());
    });

    Ok(apm_rx)
}

struct ChildShutdown {
    // Why i32? See https://github.com/nix-rust/nix/issues/656
    pid: i32,
    finished_rx: oneshot::Receiver<()>,
}

impl ChildShutdown {
    /// Politely terminate a process.
    async fn terminate(mut self) -> Result<()> {
        match self.finished_rx.try_recv() {
            Ok(()) => {
                // Already finished.
                return Ok(());
            }
            Err(TryRecvError::Closed) => {
                // lldb task somehow died before it sent a finished message.
                // Probably can't happen.
                return Err(anyhow!(TryRecvError::Closed));
            }
            Err(TryRecvError::Empty) => {
                // Hasn't finished yet. Keep going.
            }
        }

        nix::sys::signal::kill(
            nix::unistd::Pid::from_raw(self.pid),
            nix::sys::signal::SIGKILL,
        )
        .map_err(|e| anyhow!(e))?;
        self.finished_rx.await.map_err(|e| anyhow!(e))
    }
}

/// Get the PID of the first running StarCraft process, if there is one.
fn find_starcraft_pid() -> Option<Pid> {
    let system =
        System::new_with_specifics(RefreshKind::new().with_processes(ProcessRefreshKind::new()));
    system.processes().iter().find_map(|(pid, process)| {
        if process.exe().file_name() == Some(&OsString::from("StarCraft")) {
            Some(*pid)
        } else {
            None
        }
    })
}

async fn stop_all_vibrators(client: Arc<Mutex<ButtplugClient>>) {
    info!("Stopping all vibrators…");
    if let Err(e) = client.lock().await.stop_all_devices().await {
        error!("Error stopping all vibrators: {e:?}");
    }
    info!("Stopped all vibrators.");
}

async fn apm_changed(args: &Args, apm: i32, client: Arc<Mutex<ButtplugClient>>) {
    let apm_range = (args.max_apm - args.min_apm) as f64;
    let level = ((apm - args.min_apm) as f64 / apm_range).clamp(0f64, 1f64);
    info!("APM {apm} mapped to vibration level {level}");

    let client = client.lock().await;

    if !client.connected() {
        return;
    }

    for vibrator in client
        .devices()
        .iter()
        .filter(|device| device.is_vibrator())
    {
        let vibrator = vibrator.clone();
        // Send vibration commands in parallel.
        let _ = spawn(async move {
            if let Err(e) = vibrator.vibrate(&VibrateCommand::Speed(level)).await {
                error!(
                    "Error sending vibration command to {name}: {e:?}",
                    name = vibrator.name()
                );
            }
        });
    }
}
