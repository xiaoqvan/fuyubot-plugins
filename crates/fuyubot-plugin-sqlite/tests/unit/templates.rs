use super::*;

#[test]
fn renders_both_placeholders() {
    let log = find("log").expect("log 模板");
    let rendered = render(log, "demo_log");
    assert!(rendered[0].contains("\"demo_log\""));
    assert!(rendered[1].contains("\"demo_log_created\""));
    assert!(!rendered.iter().any(|sql| sql.contains('{')));
}

#[test]
fn template_names_are_unique() {
    let mut names = template_names();
    let total = names.len();
    names.sort_unstable();
    names.dedup();
    assert_eq!(names.len(), total);
}
