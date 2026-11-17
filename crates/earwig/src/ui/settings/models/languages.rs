//! Languages by name, for the Language choice and the search box.

use gpui_kit::SharedString;

/// The Language choice that leaves the language to the model.
pub(super) const DETECT_LANGUAGE: &str = "\u{0}detect";

/// Detect, then the model's languages by name, then a configured one it doesn't list.
pub(super) fn language_options(
    languages: &[String],
    configured: Option<&str>,
) -> Vec<(SharedString, SharedString)> {
    let mut named: Vec<(SharedString, SharedString)> = languages
        .iter()
        .map(|code| (code.clone().into(), language_name(code).to_owned().into()))
        .collect();
    named.sort_by(|a, b| a.1.cmp(&b.1));
    let mut options = vec![(DETECT_LANGUAGE.into(), "Detect automatically".into())];
    options.extend(named);
    if let Some(code) = configured
        && !languages.iter().any(|language| language == code)
    {
        options.push((
            code.to_owned().into(),
            format!("{} (not for this model)", language_name(code)).into(),
        ));
    }
    options
}

/// The English name of a catalog language code; others as they are.
pub(super) fn language_name(code: &str) -> &str {
    match code {
        "ar" => "Arabic",
        "bg" => "Bulgarian",
        "cs" => "Czech",
        "da" => "Danish",
        "de" => "German",
        "el" => "Greek",
        "en" => "English",
        "es" => "Spanish",
        "et" => "Estonian",
        "fa" => "Persian",
        "fi" => "Finnish",
        "fil" => "Filipino",
        "fr" => "French",
        "hi" => "Hindi",
        "hr" => "Croatian",
        "hu" => "Hungarian",
        "id" => "Indonesian",
        "it" => "Italian",
        "ja" => "Japanese",
        "ko" => "Korean",
        "lt" => "Lithuanian",
        "lv" => "Latvian",
        "mk" => "Macedonian",
        "ms" => "Malay",
        "mt" => "Maltese",
        "nb" => "Norwegian",
        "nl" => "Dutch",
        "pl" => "Polish",
        "pt" => "Portuguese",
        "ro" => "Romanian",
        "ru" => "Russian",
        "sk" => "Slovak",
        "sl" => "Slovenian",
        "sv" => "Swedish",
        "th" => "Thai",
        "tr" => "Turkish",
        "uk" => "Ukrainian",
        "vi" => "Vietnamese",
        "yue" => "Cantonese",
        "zh" => "Chinese",
        _ => code,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::settings::models::tests::catalog;

    #[test]
    fn languages_are_listed_by_name_after_detection() {
        let languages = ["fr".to_owned(), "de".to_owned(), "en".to_owned()];
        let labels = |configured| {
            language_options(&languages, configured)
                .into_iter()
                .map(|(_, label)| label.to_string())
                .collect::<Vec<_>>()
        };
        assert_eq!(
            labels(None),
            ["Detect automatically", "English", "French", "German"]
        );
        assert_eq!(labels(Some("de")).len(), 4, "a listed choice adds nothing");
        assert_eq!(
            labels(Some("ja")).last().unwrap(),
            "Japanese (not for this model)"
        );
    }

    #[test]
    fn every_catalog_language_has_a_name() {
        for entry in catalog() {
            for code in &entry.languages {
                assert_ne!(language_name(code), code, "{} lists {code}", entry.id);
            }
        }
    }
}
