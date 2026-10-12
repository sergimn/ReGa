//! ReGa signs in to AWS with the access key the user entered and nothing
//! else, even when the computer has other credentials and settings around.
//! Its own test binary, since it changes the process environment.

use std::path::Path;

use aws_credential_types::provider::ProvideCredentials;
use rega_core::aws::sdk::{sdk_config, AccessKey};
use rega_core::aws::{HOME_REGION, TERRAFORM_ISOLATION};

#[tokio::test]
async fn the_sdk_uses_the_typed_key_and_nothing_from_the_computer() {
    // What a computer might have lying around, such as an old employer's
    // profile. None of it may be used.
    let home = tempfile::tempdir().unwrap();
    let aws_dir = home.path().join(".aws");
    std::fs::create_dir(&aws_dir).unwrap();
    std::fs::write(
        aws_dir.join("credentials"),
        "[default]\naws_access_key_id = from-the-file\naws_secret_access_key = from-the-file\n",
    )
    .unwrap();
    std::fs::write(
        aws_dir.join("config"),
        "[default]\nregion = us-west-2\nendpoint_url = https://elsewhere.invalid\n",
    )
    .unwrap();
    for (k, v) in [
        ("HOME", home.path().to_str().unwrap()),
        ("AWS_ACCESS_KEY_ID", "from-the-environment"),
        ("AWS_SECRET_ACCESS_KEY", "from-the-environment"),
        ("AWS_PROFILE", "default"),
        ("AWS_REGION", "us-west-2"),
        ("AWS_DEFAULT_REGION", "us-west-2"),
        ("AWS_ENDPOINT_URL", "https://elsewhere.invalid"),
    ] {
        std::env::set_var(k, v);
    }

    let key = AccessKey {
        id: "typed-key-id".into(),
        secret: format!("typed-{}", std::process::id()),
    };
    let config = sdk_config(&key);
    let creds = config
        .credentials_provider()
        .expect("credentials")
        .provide_credentials()
        .await
        .unwrap();
    assert_eq!(creds.access_key_id(), "typed-key-id");
    assert_eq!(creds.secret_access_key(), key.secret);
    assert_eq!(creds.session_token(), None);
    assert_eq!(config.region().map(|r| r.as_ref()), Some(HOME_REGION));
    assert_eq!(config.endpoint_url(), None);

    // A client made from it agrees.
    let sts = aws_sdk_sts::config::Config::from(&config);
    assert_eq!(sts.region().map(|r| r.as_ref()), Some(HOME_REGION));
}

#[test]
fn terraform_is_kept_off_shared_files_and_instance_metadata() {
    let set = |k: &str| {
        TERRAFORM_ISOLATION
            .iter()
            .find(|(n, _)| *n == k)
            .map(|(_, v)| *v)
    };
    // Empty files in place of ~/.aws/config and ~/.aws/credentials.
    for k in ["AWS_CONFIG_FILE", "AWS_SHARED_CREDENTIALS_FILE"] {
        let path = set(k).unwrap_or_else(|| panic!("{k} isn't set"));
        assert_eq!(std::fs::read(Path::new(path)).unwrap(), b"");
    }
    assert_eq!(set("AWS_EC2_METADATA_DISABLED"), Some("true"));
}
