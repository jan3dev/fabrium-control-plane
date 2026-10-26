//! Instance type rules shared by the config check, the AWS adapter, and the handlers.

/// Families a tenant may launch.
pub const FAMILIES: [&str; 5] = ["t3", "t3a", "c6a", "m6a", "r6a"];

/// The text before the first `.`. `None` if there is no `.` or a side is empty.
pub fn family(instance_type: &str) -> Option<&str> {
    let (family, size) = instance_type.split_once('.')?;
    (!family.is_empty() && !size.is_empty()).then_some(family)
}

/// True for the families that have AMD SEV-SNP.
pub fn sev_snp(instance_type: &str) -> bool {
    matches!(family(instance_type), Some("c6a" | "m6a" | "r6a"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn family_is_the_text_before_the_first_dot() {
        assert_eq!(family("t3a.medium"), Some("t3a"));
        assert_eq!(family("m6a.large.extra"), Some("m6a"));
        for bad in ["t3", "t3a.", ".large", "", "."] {
            assert_eq!(family(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn sev_snp_is_true_only_for_c6a_m6a_r6a() {
        for yes in ["c6a.large", "m6a.large", "r6a.xlarge"] {
            assert!(sev_snp(yes), "{yes}");
        }
        for no in ["t3.medium", "t3a.large", "m6i.large", "c6a", ""] {
            assert!(!sev_snp(no), "{no}");
        }
    }
}
