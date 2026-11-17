//! Whether an installer comes from this earwig's publisher, apart from the
//! Windows calls that read the signatures so that every OS tests it.

/// What Windows makes of a file's Authenticode signature.
pub(super) enum Signature {
    Unsigned,
    /// One Windows rejects, and why.
    Invalid(String),
    /// Windows could not learn whether a certificate in it was revoked, as
    /// when the certificate authority does not answer.
    Unchecked,
    Valid(Publisher),
}

/// Who signed a file, by an Authenticode signature Windows accepts.
pub(super) struct Publisher {
    /// The signing certificate's name.
    pub(super) name: String,
    /// The signing certificate's extended key usages.
    pub(super) ekus: Vec<String>,
    /// The SHA-256 of the root certificate its chain ends at, and its name.
    pub(super) root: [u8; 32],
    pub(super) root_name: String,
}

impl Publisher {
    /// Its Artifact Signing Public Trust certificate profiles: the certificates
    /// of a profile, a new one every day, carry its own EKU right under
    /// 1.3.6.1.4.1.311.97, whose .1 holds the kinds of profile (.1.0 is Public
    /// Trust) and the other kinds' EKUs.
    fn profiles(&self) -> impl Iterator<Item = &str> {
        self.ekus.iter().map(String::as_str).filter(|eku| {
            eku.strip_prefix("1.3.6.1.4.1.311.97.")
                .is_some_and(|rest| !rest.is_empty() && rest.split('.').next() != Some("1"))
        })
    }

    fn root(&self) -> String {
        format!("{} ({})", self.root_name, hex::encode(&self.root[..8]))
    }
}

/// Whether an installer whose signature is `installer` may update an earwig
/// signed as `running`: a signed earwig takes only its own publisher's
/// ([`same_publisher`]); an unsigned one, a release from before they were
/// signed or a build, takes it on its SHA-256 alone. A refusal says why and,
/// where there is one, the way out: the installer from `page`, by hand.
pub(super) fn judge(
    running: Signature,
    installer: impl FnOnce() -> Signature,
    page: &str,
) -> Result<(), String> {
    let by_hand = || format!("download the installer from {page} and run it yourself");
    let unchecked = |whose: &str| {
        format!(
            "could not check whether {whose} signing certificate was revoked (no answer from \
             the certificate authority); {}",
            by_hand()
        )
    };
    let running = match running {
        Signature::Unsigned => {
            log::info!("this earwig is not signed, so the installer needn't be");
            return Ok(());
        }
        Signature::Invalid(why) => {
            return Err(format!(
                "this earwig's own signature is not valid ({why}), so it can't tell its \
                 publisher's installers from others"
            ));
        }
        Signature::Unchecked => return Err(unchecked("this earwig's")),
        Signature::Valid(publisher) => publisher,
    };
    let new = match installer() {
        Signature::Unsigned => {
            return Err(format!(
                "the installer is not signed, unlike this earwig ({})",
                running.name
            ));
        }
        Signature::Invalid(why) => {
            return Err(format!("the installer's signature is not valid: {why}"));
        }
        Signature::Unchecked => return Err(unchecked("the installer's")),
        Signature::Valid(publisher) => publisher,
    };
    same_publisher(&running, &new)
        .map_err(|why| format!("{why}; if you trust it, {}", by_hand()))?;
    log::info!(
        "the installer is signed by {} under {}, as this earwig is",
        new.name,
        new.root()
    );
    Ok(())
}

/// Why `new` isn't signed by the publisher of `running`, if it isn't: it must
/// chain to the same root and carry each of `running`'s certificate profiles,
/// which only the profile's own certificates from that root's authority do.
/// A subject is just a name that anyone can give their own certificate.
fn same_publisher(running: &Publisher, new: &Publisher) -> Result<(), String> {
    let ours: Vec<&str> = running.profiles().collect();
    if ours.is_empty() {
        return Err(format!(
            "this earwig's signing certificate ({}) names no Artifact Signing certificate \
             profile, so it can't tell its publisher's installers from others",
            running.name
        ));
    }
    if new.root != running.root {
        return Err(format!(
            "the installer's signature ({}) chains to another root certificate, {}, not {}",
            new.name,
            new.root(),
            running.root()
        ));
    }
    let theirs: Vec<&str> = new.profiles().collect();
    match ours.iter().find(|profile| !theirs.contains(profile)) {
        Some(profile) => Err(format!(
            "the installer is signed by {} under another certificate profile ({}), \
             not this earwig's {profile}",
            new.name,
            if theirs.is_empty() {
                "none".to_owned()
            } else {
                theirs.join(", ")
            }
        )),
        None => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // The EKUs of a real Artifact Signing certificate (python.org's installer,
    // October 2026), and of one with Microsoft's example of another profile.
    const CODE_SIGNING: &str = "1.3.6.1.5.5.7.3.3";
    const PUBLIC_TRUST: &str = "1.3.6.1.4.1.311.97.1.0";
    const PROFILE: &str = "1.3.6.1.4.1.311.97.608394634.79987812.305991749.578777327";
    const OURS: &[&str] = &[PUBLIC_TRUST, CODE_SIGNING, PROFILE];
    const THEIRS: &[&str] = &[
        PUBLIC_TRUST,
        CODE_SIGNING,
        "1.3.6.1.4.1.311.97.990309390.766961637.194916062.941502583",
    ];

    fn publisher(name: &str, ekus: &[&str], root: u8) -> Publisher {
        Publisher {
            name: name.to_owned(),
            ekus: ekus.iter().map(|&eku| eku.to_owned()).collect(),
            root: [root; 32],
            root_name: format!("root {root}"),
        }
    }

    #[test]
    fn an_installer_from_the_same_certificate_profile_and_root_is_the_publishers() {
        let running = publisher("Some Publisher", OURS, 1);
        // Another day's certificate: a new key and thumbprint, the profile's EKU.
        let next = publisher("Some Publisher", &[CODE_SIGNING, PROFILE], 1);
        assert_eq!(same_publisher(&running, &next), Ok(()));
        // A name changed with a renewed identity validation, a marker added.
        let marked = [OURS, &["1.3.6.1.4.1.311.97.1.9"]].concat();
        let later = publisher("Some J. Publisher", &marked, 1);
        assert_eq!(same_publisher(&running, &later), Ok(()));
    }

    #[test]
    fn an_installer_from_anyone_else_is_refused() {
        let running = publisher("Some Publisher", OURS, 1);
        // Another certificate profile from the same authority: another
        // subscriber, or this one's profile deleted and made anew.
        let other = publisher("Someone Else", THEIRS, 1);
        let err = same_publisher(&running, &other).unwrap_err();
        assert!(err.contains("another certificate profile"), "{err}");
        // The same name and EKUs under another root: a look-alike, self-signed
        // or from another authority.
        let look_alike = publisher("Some Publisher", OURS, 2);
        let err = same_publisher(&running, &look_alike).unwrap_err();
        assert!(err.contains("another root certificate"), "{err}");
        let plain = publisher("Some Publisher", &[CODE_SIGNING], 1);
        let err = same_publisher(&running, &plain).unwrap_err();
        assert!(err.contains("another certificate profile (none)"), "{err}");
    }

    #[test]
    fn only_a_public_trust_profile_eku_names_a_publisher() {
        let installer = publisher("Some Publisher", OURS, 1);
        for ekus in [
            &[CODE_SIGNING][..],
            &[CODE_SIGNING, PUBLIC_TRUST],
            &[CODE_SIGNING, "1.3.6.1.4.1.311.97.1.3.1.29433.35007"],
            &[CODE_SIGNING, "1.3.6.1.4.1.311.970.608394634"],
            &[CODE_SIGNING, "1.3.6.1.4.1.311.97."],
        ] {
            let running = publisher("Some Publisher", ekus, 1);
            let err = same_publisher(&running, &installer).unwrap_err();
            assert!(
                err.contains("names no Artifact Signing certificate profile"),
                "{ekus:?}: {err}"
            );
        }
    }

    #[test]
    fn a_refusal_says_why_and_where_there_is_one_the_way_out() {
        const PAGE: &str = "https://github.com/adeci/earwig/releases/tag/v0.2.0";
        let by_hand = format!("download the installer from {PAGE} and run it yourself");
        let ours = || Signature::Valid(publisher("Some Publisher", OURS, 1));
        // The certificate authority does not answer, for this earwig or the
        // installer.
        let unchecked = judge(Signature::Unchecked, || Signature::Unsigned, PAGE).unwrap_err();
        assert!(
            unchecked.starts_with("could not check whether this earwig's signing certificate"),
            "{unchecked}"
        );
        assert!(unchecked.ends_with(&by_hand), "{unchecked}");
        let unchecked = judge(ours(), || Signature::Unchecked, PAGE).unwrap_err();
        assert!(
            unchecked.starts_with("could not check whether the installer's signing certificate"),
            "{unchecked}"
        );
        assert!(unchecked.ends_with(&by_hand), "{unchecked}");
        // Another publisher's installer.
        let look_alike = || Signature::Valid(publisher("Some Publisher", OURS, 2));
        let theirs = judge(ours(), look_alike, PAGE).unwrap_err();
        assert!(theirs.contains("another root certificate"), "{theirs}");
        assert!(theirs.ends_with(&by_hand), "{theirs}");
        // A signature Windows rejects has no way out.
        let invalid = judge(ours(), || Signature::Invalid("revoked".into()), PAGE).unwrap_err();
        assert!(!invalid.contains(PAGE), "{invalid}");
        // The publisher's own, and anything for an unsigned earwig.
        assert_eq!(judge(ours(), ours, PAGE), Ok(()));
        assert_eq!(judge(Signature::Unsigned, look_alike, PAGE), Ok(()));
    }
}
