use lindiction::config::InjectionMethod;
use lindiction::inject::Injector;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let inj = Injector::new(InjectionMethod::Type, 5, "ctrl+v".to_string());
    inj.inject("hello from lindiction smoke_inject").await?;
    Ok(())
}
