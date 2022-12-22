use anyhow::{anyhow, bail, Result};
use buttplug::client::{ButtplugClient, ButtplugClientDevice, VibrateCommand};
use buttplug::core::connector::{ButtplugRemoteClientConnector, ButtplugWebsocketClientTransport};
use buttplug::core::message::serializer::ButtplugClientJSONSerializer;
use buttplug::core::message::ActuatorType;
use clap::Parser;
use std::ffi::OsString;
use std::io::Write;
use std::process::Stdio;
use tempfile;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;
use tokio::sync::watch;
use tracing::{error, info};

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

    info!("Connecting to Intiface…");
    let client = connect_to_buttplug(args.server).await?;
    info!("Connected to Intiface.");

    info!("Starting lldb and waiting for StarCraft to be launched…");
    let apm_rx = connect_to_starcraft(args.show_lldb_errors).await?;
    info!("lldb started.");

    sync_apm_to_vibrators(args.min_apm, args.max_apm, apm_rx, client).await;

    Ok(())
}

/// Connect to an Intiface server.
async fn connect_to_buttplug(server: String) -> Result<ButtplugClient> {
    let connector = ButtplugRemoteClientConnector::<
        ButtplugWebsocketClientTransport,
        ButtplugClientJSONSerializer,
    >::new(ButtplugWebsocketClientTransport::new_insecure_connector(
        &server,
    ));
    let client = ButtplugClient::new("StarPlug");
    client.connect(connector).await?;
    client.start_scanning().await?;
    Ok(client)
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

/// Python script that we ask `lldb` to run.
/// Writes status lines like `APM: 69`.
const STARPLUG_PY: &[u8] = include_bytes!("starplug.py");

/// Launch `lldb` with our instrumentation script,
/// wait for Starcraft to be launched,
/// and start tracking APM.
async fn connect_to_starcraft(show_lldb_errors: bool) -> Result<watch::Receiver<i32>> {
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
    let mut lldb = Command::new("lldb")
        .args(["--batch", "--source-quietly", "--one-line"])
        .arg(script_arg)
        .stdout(Stdio::piped())
        .stderr(if show_lldb_errors {
            Stdio::inherit()
        } else {
            Stdio::null()
        })
        .spawn()?;

    let lldb_stdout = lldb
        .stdout
        .take()
        .ok_or(anyhow!("Couldn't get lldb's stdout!"))?;
    let mut lldb_reader = BufReader::new(lldb_stdout).lines();

    let (apm_tx, apm_rx) = watch::channel(0i32);

    // Spawn a task to watch for APM info from lldb.
    let _ = tokio::spawn(async move {
        let mut prev = 0i32;
        while let Ok(Some(line)) = lldb_reader.next_line().await {
            if let Some(apm_str_ws) = line.strip_prefix("APM:") {
                if let Ok(apm) = apm_str_ws.trim().parse::<i32>() {
                    if apm == prev {
                        continue;
                    }
                    prev = apm;
                    if let Err(e) = apm_tx.send(apm) {
                        error!("Couldn't send APM through watch channel: {e:?}");
                        break;
                    }
                }
            }
        }
        info!("lldb reader task finished.");
    });

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
    });

    Ok(apm_rx)
}

/// Send vibration commands when APM changes.
async fn sync_apm_to_vibrators(
    min_apm: i32,
    max_apm: i32,
    mut apm_rx: watch::Receiver<i32>,
    client: ButtplugClient,
) {
    let apm_range = (max_apm - min_apm) as f64;
    while let Ok(()) = apm_rx.changed().await {
        let apm = *apm_rx.borrow_and_update();
        let level = ((apm - min_apm) as f64 / apm_range).clamp(0f64, 1f64);
        info!("APM {apm} mapped to vibration level {level}");
        for vibrator in client
            .devices()
            .iter()
            .filter(|device| device.is_vibrator())
        {
            let vibrator = vibrator.clone();
            // Send vibration commands in parallel.
            let _ = tokio::spawn(async move {
                if let Err(e) = vibrator.vibrate(&VibrateCommand::Speed(level)).await {
                    error!(
                        "Error sending vibration command to {name}: {e:?}",
                        name = vibrator.name()
                    );
                }
            });
        }
    }
}
