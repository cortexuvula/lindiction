use global_hotkey::hotkey::{Code, Modifiers};
use lindiction::hotkey::{start, HotkeyEvent};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt().with_env_filter("info").init();
    let (_lst, mut rx) = start(Modifiers::CONTROL | Modifiers::ALT, Code::Space)?;
    println!("Hold Ctrl+Alt+Space. Press Ctrl+C to exit.");

    loop {
        tokio::select! {
            Some(evt) = rx.recv() => match evt {
                HotkeyEvent::Press => println!("PRESS"),
                HotkeyEvent::Release => println!("RELEASE"),
            },
            _ = tokio::signal::ctrl_c() => break,
        }
    }
    Ok(())
}
