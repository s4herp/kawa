//! Folding a person's other addresses into the developer their GitHub login
//! links to.

use kawa_core::identity::{
    DeveloperId, DeveloperSpec, IdentityRules, LoginEvidence, LoginMerges, link_logins,
    noreply_login,
};

const BOT: &str = r"(?i)\[bot\]$";

fn evidence<'a>(rows: &'a [(&'a str, &'a str, u64)]) -> impl Iterator<Item = LoginEvidence<'a>> {
    rows.iter().map(|&(login, email, commits)| LoginEvidence {
        login,
        name: "Someone",
        email,
        commits,
    })
}

fn id(email: &str) -> DeveloperId {
    DeveloperId::from_email(email)
}

#[test]
fn reads_login_from_noreply_addresses() {
    assert_eq!(
        noreply_login("12345+Ana-GH@users.noreply.github.com").as_deref(),
        Some("ana-gh"),
        "should take the login after the numeric id"
    );
    assert_eq!(
        noreply_login("luis-gh@users.noreply.github.com").as_deref(),
        Some("luis-gh"),
        "should accept the old form without numeric id"
    );
    assert_eq!(
        noreply_login("ana@example.com"),
        None,
        "should ignore regular addresses"
    );
}

#[test]
fn folds_personal_and_noreply_addresses_into_the_linked_developer() {
    let rules = IdentityRules::new(&[], &[], BOT).unwrap();
    let rows = [
        ("ana-gh", "ana@work.example", 90),
        ("ana-gh", "ana.personal@mail.example", 40),
        ("ana-gh", "1+ana-gh@users.noreply.github.com", 200),
    ];
    let links = link_logins(&rules, evidence(&rows));
    assert_eq!(
        links.get("ana-gh"),
        Some(&id("ana@work.example")),
        "should link the login to a real address over a busier noreply one"
    );
    let merges = LoginMerges::new(&rules, evidence(&rows), &links);
    for email in [
        "ana.personal@mail.example",
        "1+ana-gh@users.noreply.github.com",
    ] {
        assert_eq!(
            merges.target(&rules, id(email)),
            id("ana@work.example"),
            "should fold {email} into the linked developer"
        );
    }
    assert_eq!(
        merges.target(&rules, id("2+ana-gh@users.noreply.github.com")),
        id("ana@work.example"),
        "should fold a noreply address of the login even without PR commits"
    );
}

#[test]
fn keeps_shared_addresses_with_their_main_login_and_config_developers_apart() {
    let developers = [DeveloperSpec {
        email: "lead@work.example".into(),
        ..Default::default()
    }];
    let rules = IdentityRules::new(&[], &developers, BOT).unwrap();
    let rows = [
        ("ana-gh", "ana@work.example", 50),
        ("ana-gh", "shared@work.example", 1),
        ("ana-gh", "lead@work.example", 1),
        ("luis-gh", "luis@work.example", 50),
        ("luis-gh", "shared@work.example", 9),
    ];
    let links = link_logins(&rules, evidence(&rows));
    let merges = LoginMerges::new(&rules, evidence(&rows), &links);
    assert_eq!(
        merges.target(&rules, id("shared@work.example")),
        id("luis@work.example"),
        "should give a shared address to the login it was used with most"
    );
    assert_eq!(
        merges.target(&rules, id("lead@work.example")),
        id("lead@work.example"),
        "should never fold a developer declared in config"
    );
    assert_eq!(
        merges.target(&rules, id("3+nobody@users.noreply.github.com")),
        DeveloperId::from_login("nobody"),
        "should key an unlinked noreply address by its login"
    );
}
