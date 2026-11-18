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

/// The instances a description declares: each HAL's name, and each of the
/// instances it gives under it.
///
/// Two descriptions of one instance are refused by the reader outright — it
/// drops the whole manifest over the conflict, so no service is found at all
/// — which is why this is worth reading out of the text.
pub fn declares(text: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let mut rest = text;
    let mut name: Option<String> = None;
    loop {
        let next_name = rest.find("<name>");
        let next_fqname = rest.find("<fqname>");
        let take_name = match (next_name, next_fqname) {
            (None, None) => break,
            (Some(at), None) => Some(at),
            (None, Some(_)) => None,
            (Some(n), Some(f)) => (n < f).then_some(n),
        };
        match take_name {
            Some(at) => {
                let after = &rest[at + "<name>".len()..];
                let Some(end) = after.find("</name>") else { break };
                name = Some(after[..end].trim().to_string());
                rest = &after[end + "</name>".len()..];
            }
            None => {
                let at = next_fqname.expect("one of the two was found");
                let after = &rest[at + "<fqname>".len()..];
                let Some(end) = after.find("</fqname>") else { break };
                if let Some(hal) = &name {
                    out.push((hal.clone(), after[..end].trim().to_string()));
                }
                rest = &after[end + "</fqname>".len()..];
            }
        }
    }
    out
}

/// Whether two descriptions are of one and the same instance, which a reader
/// will not have declared twice.
pub fn collide(one: &str, other: &str) -> bool {
    let theirs = declares(one);
    declares(other).iter().any(|instance| theirs.contains(instance))
}

#[cfg(test)]
mod tests {
    use super::{as_framework, collide, declares};

    /// The two descriptions this phone actually carries of one weaver: the
    /// one bundled in the ramdisk, and the one the installed system carries.
    const BUNDLED_WEAVER: &str = "<!-- The Thales weaver, for a phone whose odm carries it without a description. -->\n<manifest version=\"1.0\" type=\"framework\">\n    <hal format=\"aidl\">\n        <name>android.hardware.weaver</name>\n        <fqname>IWeaver/default</fqname>\n    </hal>\n</manifest>\n";
    const SYSTEMS_WEAVER: &str = "<manifest version=\"1.0\" type=\"framework\">\n    <hal format=\"aidl\">\n        <name>android.hardware.weaver</name>\n        <version>2</version>\n        <fqname>IWeaver/default</fqname>\n    </hal>\n</manifest>\n";

    #[test]
    fn a_description_declares_each_instance_it_gives() {
        assert_eq!(declares(BUNDLED_WEAVER), [("android.hardware.weaver".to_string(), "IWeaver/default".to_string())]);
        assert_eq!(declares(SYSTEMS_WEAVER).len(), 1);
        // One name, several instances under it.
        let both = declares(&[
            "<manifest>",
            "<hal><name>android.hardware.secure_element</name>",
            "<fqname>ISecureElement/SIM1</fqname>",
            "<fqname>ISecureElement/SIM2</fqname></hal>",
            "</manifest>",
        ].join("\n"));
        assert_eq!(both.len(), 2, "both instances, both under the one name: {both:?}");
        assert_eq!(both[0].0, "android.hardware.secure_element");
        assert_eq!(both[1].1, "ISecureElement/SIM2");
    }

    /// The conflict that stopped every service from registering on the phone:
    /// the ramdisk's older description of the weaver and the installed
    /// system's newer one are of the same instance, and the reader drops the
    /// whole manifest over it, so nothing at all is found.
    #[test]
    fn two_descriptions_of_one_instance_are_seen_to_collide() {
        assert!(collide(BUNDLED_WEAVER, SYSTEMS_WEAVER));
        assert!(collide(SYSTEMS_WEAVER, BUNDLED_WEAVER), "whichever way round");
    }

    #[test]
    fn descriptions_of_different_instances_do_not_collide() {
        let secure_element = "<manifest><hal><name>android.hardware.secure_element</name><fqname>ISecureElement/eSE1</fqname></hal></manifest>";
        let omapi = "<manifest><hal><name>android.se.omapi</name><fqname>ISecureElementService/default</fqname></hal></manifest>";
        assert!(!collide(BUNDLED_WEAVER, secure_element));
        assert!(!collide(secure_element, omapi), "the interface name alone is not enough");
        assert!(!collide(omapi, BUNDLED_WEAVER));
        assert!(!collide("<manifest/>", BUNDLED_WEAVER), "a description of nothing collides with nothing");
    }


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
