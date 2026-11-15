use super::*;

#[test]
fn line_column_counts_chars_not_bytes() {
    let source = "ab\n中文x";
    let offset = source.find('x').unwrap();
    assert_eq!(line_column(source, offset), (2, 3));
}

#[test]
fn error_clamps_offset() {
    let error = Error::new(ErrorKind::Parse, "abc", 99, "boom");
    assert_eq!(error.offset, 3);
    assert_eq!(error.line, 1);
    assert_eq!(error.column, 4);
}
