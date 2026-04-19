use lindiction::inject::Injector;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let inj = Injector::new(5);
    inj.inject("hello from lindiction smoke_inject").await?;
    Ok(())
}
