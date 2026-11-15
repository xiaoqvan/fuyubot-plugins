use super::*;

#[test]
fn roles_parse_and_compare() {
    assert_eq!(GlobalRole::parse("owner"), GlobalRole::Owner);
    assert_eq!(GlobalRole::parse(" ADMIN "), GlobalRole::Admin);
    assert_eq!(GlobalRole::parse("user"), GlobalRole::User);
    assert_eq!(GlobalRole::parse("nope"), GlobalRole::User);

    assert!(GlobalRole::Owner.is_admin());
    assert!(GlobalRole::Admin.is_admin());
    assert!(!GlobalRole::User.is_admin());

    assert!(GlobalRole::Owner.is_owner());
    assert!(!GlobalRole::Admin.is_owner());
    assert!(!GlobalRole::User.is_owner());
}
