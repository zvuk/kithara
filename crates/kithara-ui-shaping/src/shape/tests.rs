use kithara_test_utils::kithara;

use crate::{FontId, FontPolicy, TextResources};

#[kithara::test]
fn context_registers_only_embedded_families() {
    let resources = TextResources::new(FontPolicy::Embedded).unwrap();
    let mut collection = resources.collection();
    let mut families: Vec<String> = collection.family_names().map(ToOwned::to_owned).collect();
    families.sort();

    assert_eq!(
        families,
        ["Inter", "JetBrains Mono", "Space Grotesk", "lucide"],
        "the harness policy must register the ten embedded faces and exclude machine-owned families"
    );
    assert_eq!(
        FontId::ALL,
        [
            FontId::InterRegular,
            FontId::InterSemibold,
            FontId::JetBrainsMonoRegular,
            FontId::JetBrainsMonoMedium,
            FontId::JetBrainsMonoSemibold,
            FontId::SpaceGroteskRegular,
            FontId::SpaceGroteskMedium,
            FontId::SpaceGroteskSemibold,
            FontId::SpaceGroteskBold,
            FontId::Lucide,
        ],
        "all ten registered embedded faces are named by the catalog contract"
    );
}
