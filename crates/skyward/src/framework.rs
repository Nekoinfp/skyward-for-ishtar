//! The description of what the phone's hardware services are, brought down to
//! the version a recovery's own reader knows.
//!
//! A description that names a version the reader does not know is not read
//! alone: the reader drops every description there is, so no service
//! registers at all and the data partition never opens. That is why this is
//! kept out of the phone-only module: what it does is plain text, and it is
//! worth a test that runs.

/// The oldest version a description can name, which every reader knows.
const OLDEST_VERSION: &str = "1.0";

/// The description of a phone's own hardware, said as a framework's so that a
/// recovery's service directory takes it.
pub fn as_framework(text: &str) -> String {
    let text = text.replace("type=\"device\"", "type=\"framework\"").replace("type='device'", "type='framework'");
    oldest_version(&text)
}

/// The text with the version on its `<manifest …>` line made
/// [`OLDEST_VERSION`], or as it is when it names that version already or
/// names none.
fn oldest_version(text: &str) -> String {
    const SAID: &str = "version=";
    let Some(start) = text.find("<manifest") else { return text.to_string() };
    let Some(element) = text[start..].split_once('>').map(|(element, _)| element) else { return text.to_string() };
    let Some(at) = element.find(SAID) else { return text.to_string() };
    let rest = &element[at + SAID.len()..];
    let Some(quote) = rest.chars().next().filter(|c| *c == '"' || *c == '\'') else { return text.to_string() };
    let Some(end) = rest[1..].find(quote).map(|e| e + 1) else { return text.to_string() };
    if rest[1..end] == *OLDEST_VERSION {
        return text.to_string();
    }
    let from = start + at;
    let to = start + at + SAID.len() + end + 1;
    format!("{}version={quote}{OLDEST_VERSION}{quote}{}", &text[..from], &text[to..])
}

#[cfg(test)]
mod tests {
    use super::as_framework;

    #[test]
    fn a_description_of_the_phones_own_hardware_is_taken_as_the_frameworks() {
        assert_eq!(
            as_framework("<manifest version=\"8.0\" type=\"device\">\n"),
            "<manifest version=\"1.0\" type=\"framework\">\n"
        );
        assert_eq!(
            as_framework("<manifest version='8.0' type='device'>"),
            "<manifest version='1.0' type='framework'>"
        );
    }

    #[test]
    fn a_description_written_for_a_newer_platform_is_read_as_the_oldest_version() {
        // This is the one that stopped the boot: the description the
        // recovery carries for se_omapi says it was written for a version
        // the recovery's own reader does not know, and the reader then drops
        // every description there is, so no service registers at all.
        assert_eq!(
            as_framework("<manifest version=\"9.0\" type=\"framework\">\n"),
            "<manifest version=\"1.0\" type=\"framework\">\n"
        );
        assert_eq!(
            as_framework("<manifest version='9.0' type='framework'>"),
            "<manifest version='1.0' type='framework'>"
        );
    }

    #[test]
    fn a_description_that_already_names_the_oldest_version_is_left_as_it_is() {
        for text in [
            "<manifest version=\"1.0\" type=\"framework\"><hal/></manifest>\n",
            "<manifest type=\"framework\"><hal/></manifest>\n",
        ] {
            assert!(as_framework(text) == text, "{text}");
        }
    }

    #[test]
    fn only_the_version_of_the_description_itself_is_brought_down() {
        let said = "<!-- version=\"9.0\" -->\n<manifest version=\"7.0\" type=\"framework\">\n    <hal version=\"4.0\"/>\n</manifest>\n";
        let want = "<!-- version=\"9.0\" -->\n<manifest version=\"1.0\" type=\"framework\">\n    <hal version=\"4.0\"/>\n</manifest>\n";
        assert_eq!(as_framework(said), want);
    }
}
