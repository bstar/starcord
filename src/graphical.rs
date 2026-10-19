//! KIT owns the frontend/session transport; CORD owns its controller and data.
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use clap::Parser;
use starkit::graphics::{Graphics, Mode};
use starkit::terminal_graphics::{
    client::{self, Launch, PresentationOptions, PresentationPreference},
    protocol::{Input, Viewport},
    session::{self, Controller},
};

use crate::ui::{app::native::Native, app::App, fake};
use crate::{cli, discord::Handle, load_config, paths::PATHS};

pub fn internal_args() -> bool {
    matches!(
        std::env::args().nth(1).as_deref(),
        Some("--serve" | "--graphical-relay" | "--sessions" | "--attach")
    )
}

pub fn run() -> Result<()> {
    let root = PATHS.runtime_dir()?.join("native");
    session::private_root(&root)?;
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("--sessions") => {
            for name in session::list(&root)? {
                println!("{name}");
            }
            Ok(())
        }
        Some("--serve") => {
            let name = args.get(1).context("--serve requires a session name")?;
            let context = context_path(&root, name)?;
            let launch: LaunchContext = serde_json::from_slice(&std::fs::read(&context)?)?;
            struct Cleanup(PathBuf);
            impl Drop for Cleanup {
                fn drop(&mut self) {
                    let _ = std::fs::remove_file(&self.0);
                }
            }
            let _cleanup = Cleanup(context);
            session::serve(&root, name, controller(launch.replay)?)
        }
        Some("--graphical-relay") => {
            let name = args.get(1).context("Missing graphical session name")?;
            let result = ensure(&root, name, args.iter().any(|a| a == "--attach-only"))
                .and_then(|socket| session::relay(&socket));
            if let Err(error) = result {
                use std::io::Write;
                starkit::terminal_graphics::protocol::write_message(
                    &starkit::terminal_graphics::protocol::ServerMessage::Error {
                        message: format!("{error:#}"),
                    },
                    &mut std::io::stdout().lock(),
                )?;
                std::io::stdout().flush()?;
                // Keep the error available until the frontend consumes it.
                let _ = std::io::copy(&mut std::io::stdin().lock(), &mut std::io::sink());
            }
            Ok(())
        }
        Some("--attach") => {
            let name = args.get(1).context("--attach requires a session name")?;
            anyhow::ensure!(
                session::socket_path(&root, name)?.exists(),
                "Session is no longer running"
            );
            frontend(&root, name, true, false)
        }
        _ => {
            let options =
                cli::Cli::try_parse_from(std::iter::once("starcord".to_owned()).chain(args))?;
            let replay = options
                .replay
                .map(|p| {
                    if p.is_absolute() {
                        Ok(p)
                    } else {
                        std::env::current_dir().map(|d| d.join(p))
                    }
                })
                .transpose()?;
            let stamp = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)?
                .as_millis();
            let name = format!("cord-{:x}-{stamp:x}", std::process::id());
            let context = context_path(&root, &name)?;
            starkit::fs::write_private(&context, &serde_json::to_vec(&LaunchContext { replay })?)?;
            let result = frontend(&root, &name, false, options.graphical);
            if !session::socket_path(&root, &name)?.exists() {
                let _ = std::fs::remove_file(context);
            }
            result
        }
    }
}

#[derive(serde::Serialize, serde::Deserialize)]
struct LaunchContext {
    replay: Option<PathBuf>,
}

fn context_path(root: &Path, name: &str) -> Result<PathBuf> {
    session::socket_path(root, name)?;
    Ok(root.join(format!("{name}.json")))
}

fn frontend(root: &Path, name: &str, attach: bool, graphical: bool) -> Result<()> {
    let preference_path = PATHS.base_dir()?.join("presentation.toml");
    let preference = std::fs::read_to_string(&preference_path)
        .ok()
        .and_then(|s| toml::from_str::<toml::Value>(&s).ok());
    let cells = !graphical
        && preference
            .as_ref()
            .and_then(|p| p.get("ui")?.get("presentation")?.as_str())
            == Some("cells");
    if let Some(bridge) = starkit::terminal_graphics::terminal_bridge::probe()? {
        return bridge.relay_with_presentation(
            &ensure(root, name, attach)?,
            None,
            graphical.then_some(false),
        );
    }
    client::run_with_preference(
        Launch {
            executable: std::env::current_exe()?.to_string_lossy().into_owned(),
            host: None,
            ssh_config: None,
            session: name.into(),
            directory: None,
            attach_only: attach,
            play: None,
        },
        terminal_event,
        || None,
        PresentationOptions {
            pane_corner_radius: 0,
            video_corner_radius: 0,
        },
        PresentationPreference {
            cells,
            path: preference_path,
        },
    )
}

fn terminal_event(event: &starkit::crossterm::event::Event) -> Option<Input> {
    matches!(event, starkit::crossterm::event::Event::FocusGained).then(|| Input::Notice {
        message: "starcord:focus-gained".into(),
    })
}

fn ensure(root: &Path, name: &str, attach: bool) -> Result<PathBuf> {
    let socket = session::socket_path(root, name)?;
    if std::os::unix::net::UnixStream::connect(&socket).is_ok() {
        return Ok(socket);
    }
    anyhow::ensure!(!attach, "Session is no longer running");
    anyhow::ensure!(
        context_path(root, name)?.exists(),
        "Missing session launch context"
    );
    // A persistent AppImage host needs its own mount lifetime.
    let image = std::env::var_os("APPIMAGE").filter(|_| cfg!(target_os = "linux"));
    let executable = image
        .as_ref()
        .map(PathBuf::from)
        .unwrap_or(std::env::current_exe()?);
    let mut command = std::process::Command::new(executable);
    if image.is_some() {
        command.env_remove("APPIMAGE").env_remove("APPDIR");
    }
    use std::os::unix::process::CommandExt;
    let mut child = command
        .args(["--serve", name])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .process_group(0)
        .spawn()?;
    let started = Instant::now();
    loop {
        if std::os::unix::net::UnixStream::connect(&socket).is_ok() {
            return Ok(socket);
        }
        if let Some(status) = child.try_wait()? {
            anyhow::bail!("Graphical controller exited ({status}); see STAR/CORD's log");
        }
        if started.elapsed() > Duration::from_secs(10) {
            let _ = child.kill();
            let _ = child.wait();
            anyhow::bail!("Timed out starting the graphical controller");
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn controller(replay: Option<PathBuf>) -> Result<Native> {
    let (cfg, config_path) = load_config()?;
    let core = match &replay {
        Some(path) => fake::replay(path).context("starting the replay core")?,
        None => Handle::spawn(cfg.core(), PATHS).context("starting the Discord core")?,
    };
    let session = replay.is_none().then(|| PATHS.session_file()).transpose()?;
    let mut graphics = Graphics::disabled();
    if Mode::parse(&cfg.ui.graphics) != Mode::Off {
        graphics.set_mode(Mode::Blocks);
    }
    Ok(Native::new(App::new(
        core,
        cfg,
        config_path,
        session,
        graphics,
    )))
}

/// A deterministic preview never loads credentials, starts Discord or saves a session.
pub fn render(replay: &Path, output: &Path) -> Result<()> {
    let fixture = fake::Session::read(replay)?;
    let (core, mut idle) = fake::loaded(&fixture, 1_000);
    let preview_channel = {
        let state = core.state();
        state
            .guilds_ordered()
            .into_iter()
            .flat_map(|guild| state.channels_ordered(guild.id))
            .find(|channel| !state.recent(channel.id, 1).is_empty())
            .map(|channel| channel.id)
    };
    let (cfg, config_path) = load_config()?;
    let mut graphics = Graphics::disabled();
    if Mode::parse(&cfg.ui.graphics) != Mode::Off {
        graphics.set_mode(Mode::Blocks);
    }
    let mut app = App::new(core, cfg, config_path, None, graphics);
    app.tz = jiff::tz::TimeZone::UTC;
    app.tick();
    // Walk the fixture through the ordinary server/channel navigation. This
    // happens only in the offline export, never in an account session.
    use starkit::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    app.key(KeyEvent::new(KeyCode::Char('1'), KeyModifiers::ALT));
    app.key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
    app.key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    app.tick();
    if let Some(channel) =
        preview_channel.or_else(|| app.view.channels.iter().find_map(|row| row.channel_id()))
    {
        app.open_channel(channel);
        app.tick();
        app.layout
            .focus_set(crate::ui::panels::ModuleId::Conversation);
    }
    let mut native = Native::new(app);
    for _ in 0..3 {
        native.scene(Viewport::default());
        idle.pump();
        native.tick();
    }
    let scene = native.scene(Viewport::default());
    starkit::terminal_graphics::renderer::render_reference(&scene)?.save(output)?;
    Ok(())
}
