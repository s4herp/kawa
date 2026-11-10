//! The example config in the README must stay valid.

use std::fs;
use std::path::Path;

use kawa::config::Config;

#[test]
fn readme_example_config_parses() {
    let readme = fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../README.md"))
        .expect("README exists at the workspace root");
    let block = readme
        .split("```toml\n")
        .nth(1)
        .and_then(|rest| rest.split("\n```").next())
        .expect("README has a toml block");
    let config = Config::parse(block).expect("README example config is valid");
    assert_eq!(
        config.developers[0].logins,
        vec!["ana-gh".to_owned()],
        "should read the documented developer"
    );
    assert_eq!(
        config.repositories[0].github.as_deref(),
        Some("acme/app"),
        "should read the documented repository"
    );
}
