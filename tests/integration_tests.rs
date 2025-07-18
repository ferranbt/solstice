use solstice::TraceArgs;
use std::fs;
use std::path::PathBuf;
use tokio::process::Command;

async fn get_external_dir() -> eyre::Result<PathBuf> {
    let current_dir = std::env::current_dir()?;
    let external_dir = current_dir.join("external");

    // Create the directory if it doesn't exist
    fs::create_dir_all(&external_dir)?;

    Ok(external_dir)
}

async fn clone_repo_if_needed(repo_url: &str, repo_name: &str) -> eyre::Result<PathBuf> {
    let external_dir = get_external_dir().await?;
    let repo_path = external_dir.join(repo_name);

    // Check if already cloned
    if repo_path.exists() {
        println!("Repository {} already exists, skipping clone", repo_name);
        return Ok(repo_path);
    }

    println!("Cloning {} to {:?}", repo_name, repo_path);

    let output = Command::new("git")
        .args(["clone", "--depth", "1", repo_url])
        .arg(&repo_path)
        .output()
        .await?;

    if !output.status.success() {
        eyre::bail!(
            "Failed to clone repo {}: {}",
            repo_name,
            String::from_utf8_lossy(&output.stderr)
        );
    }

    Ok(repo_path)
}

async fn test_repo(
    repo_url: &str,
    repo_name: &str,
    test_path: &str,
    test_name: &str,
) -> eyre::Result<()> {
    clone_repo_if_needed(repo_url, repo_name).await?;

    let args = TraceArgs {
        match_test: test_name.into(),
        match_path: test_path.into(),
        workspace: Some(format!("external/{repo_name}").into()),
        dump: Some("/tmp/dump.json".into()),
        pprof: None,
        flamegraph: None,
    };
    println!("Running test with args: {:?}", args);
    args.run()?;

    Ok(())
}

#[tokio::test]
async fn integration_tests() -> eyre::Result<()> {
    test_repo(
        "https://github.com/Uniswap/v4-core.git",
        "v4-core",
        "test/CurrencyReserves.t.sol",
        "test_getReserves_returns_set",
    )
    .await?;

    Ok(())
}
