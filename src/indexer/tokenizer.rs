use std::collections::HashSet;
use std::sync::LazyLock;

use unicode_normalization::UnicodeNormalization;
use unicode_normalization::char::is_combining_mark;

// Common Spanish and English stopwords, already accent-stripped since
// tokens are matched against this set after strip_accents runs.
// Later we can use the stopwords crate to load stopwords from a file or a more comprehensive list.
static STOPWORDS: LazyLock<HashSet<&'static str>> = LazyLock::new(|| {
    HashSet::from([
        "el", "la", "los", "las", "un", "una", "unos", "unas", "de", "del", "al", "a", "en", "y",
        "o", "que", "es", "son", "se", "su", "sus", "por", "para", "con", "sin", "no", "lo", "le",
        "les", "mi", "tu", "yo", "como", "mas", "pero", "si", // Spanish
        "the", "a", "an", "of", "to", "in", "on", "for", "and", "or", "is", "are", "was", "were",
        "be", "been", "it", "its", "this", "that", "these", "those", "with", "as", "at", "by",
        "from", "not", "but", "if", // English
    ])
});

// Tokenizer module for the Scout search engine
pub fn tokenizer(text: &str) -> Vec<String> {
    let text = strip_accents(text);

    text.to_lowercase()
        .split(|c: char| !c.is_alphanumeric() && c != '_')
        .filter(|token| !token.is_empty() && !STOPWORDS.contains(token))
        .map(ToString::to_string)
        .collect()
}

fn strip_accents(s: &str) -> String {
    s.nfd()
        .filter(|c| !is_combining_mark(*c))
        .collect::<String>()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lowercases_tokens() {
        assert_eq!(tokenizer("Hola Mundo"), vec!["hola", "mundo"]);
    }

    #[test]
    fn strips_accents() {
        assert_eq!(tokenizer("Índice café"), vec!["indice", "cafe"]);
    }

    #[test]
    fn splits_on_non_alphanumeric() {
        assert_eq!(tokenizer("Hola, mundo! 123"), vec!["hola", "mundo", "123"]);
    }

    #[test]
    fn collapses_consecutive_separators() {
        assert_eq!(tokenizer("hola   ,,, mundo"), vec!["hola", "mundo"]);
    }

    #[test]
    fn trims_leading_and_trailing_separators() {
        assert_eq!(tokenizer("  hola mundo  "), vec!["hola", "mundo"]);
    }

    #[test]
    fn empty_string_returns_no_tokens() {
        let result: Vec<String> = tokenizer("");
        assert!(result.is_empty());
    }

    #[test]
    fn only_separators_returns_no_tokens() {
        let result: Vec<String> = tokenizer("!!! ,,, ???");
        assert!(result.is_empty());
    }

    #[test]
    fn dot_treated_as_separator() {
        assert_eq!(tokenizer("archivo_v2.txt"), vec!["archivo_v2", "txt"]);
    }

    #[test]
    fn filters_spanish_stopwords() {
        assert_eq!(tokenizer("el gato y la casa"), vec!["gato", "casa"]);
    }

    #[test]
    fn filters_english_stopwords() {
        assert_eq!(tokenizer("the cat and the house"), vec!["cat", "house"]);
    }

    #[test]
    fn stopwords_only_returns_no_tokens() {
        let result: Vec<String> = tokenizer("el la de que y");
        assert!(result.is_empty());
    }
}
