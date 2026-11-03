use nexus::core::recall::{count_words, truncate_to_im_word_limit};

#[test]
fn zero_budget_never_emits_an_ellipsis() {
    for text in ["hello", "你好", " hello 世界"] {
        assert_eq!(truncate_to_im_word_limit(text, 0), "");
    }
}

#[test]
fn preserves_text_that_fits() {
    for text in ["", " \n", "hello world", "你好 world", "こんにちは"] {
        assert_eq!(truncate_to_im_word_limit(text, count_words(text)), text);
    }
}

#[test]
fn truncates_mixed_scripts_with_space_for_the_ellipsis() {
    for (text, budget, expected) in [
        ("hello world again", 2, "hello world…"),
        ("你好世界", 3, "你好…"),
        ("hello你好 world", 3, "hello你…"),
        ("你hello world", 2, "你hello…"),
        ("你好", 1, "…"),
        ("hello\n\tworld", 1, "hello…"),
    ] {
        let result = truncate_to_im_word_limit(text, budget);
        assert_eq!(result, expected);
        assert!(count_words(&result) <= budget);
    }
}

#[test]
fn handles_long_unbroken_tokens() {
    let token = "a".repeat(100_000);
    assert_eq!(
        truncate_to_im_word_limit(&format!("{token} next"), 1),
        format!("{token}…")
    );
}
