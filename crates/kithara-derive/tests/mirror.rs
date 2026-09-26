use kithara_derive::Mirror;
use kithara_test_utils::kithara;

struct Source {
    enabled: bool,
    input_name: u8,
}

#[derive(Debug, Eq, Mirror, PartialEq)]
#[mirror(from = Source)]
struct Target {
    enabled: bool,
    #[mirror(rename = input_name)]
    name: u8,
}

enum SourceEvent {
    Ready,
    Value(u8),
    Named { count: u8 },
}

#[derive(Debug, Eq, Mirror, PartialEq)]
#[mirror(from = SourceEvent)]
enum TargetEvent {
    Ready,
    Value(u8),
    Named { count: u8 },
}

enum BorrowedSource {
    Count(u8),
    Text(String),
}

#[derive(Debug, Eq, Mirror, PartialEq)]
#[mirror(from_ref = BorrowedSource)]
enum BorrowedTarget<'a> {
    Count(#[mirror(copy)] u8),
    Text(#[mirror(as_ref)] &'a str),
}

enum ExternalName {
    Renamed,
}

#[derive(Debug, Eq, PartialEq)]
struct Narrow {
    kept: u8,
}

#[derive(Mirror)]
#[mirror(into = Narrow)]
struct Wide {
    kept: u8,
    #[mirror(skip)]
    dropped: u8,
}

#[derive(Mirror)]
#[mirror(into = ExternalName)]
enum LocalName {
    #[mirror(rename = Renamed)]
    Local,
}

#[kithara::test(native, flash(false))]
fn mirrors_struct_fields_and_complete_enum_variants() {
    assert_eq!(
        Target::from(Source {
            input_name: 7,
            enabled: true,
        }),
        Target {
            name: 7,
            enabled: true,
        }
    );
    assert_eq!(TargetEvent::from(SourceEvent::Ready), TargetEvent::Ready);
    assert_eq!(
        TargetEvent::from(SourceEvent::Value(4)),
        TargetEvent::Value(4)
    );
    assert_eq!(
        TargetEvent::from(SourceEvent::Named { count: 3 }),
        TargetEvent::Named { count: 3 }
    );
}

#[kithara::test(native, flash(false))]
fn mirrors_borrowed_fields_only_with_declared_operations() {
    assert_eq!(
        BorrowedTarget::from(&BorrowedSource::Count(5)),
        BorrowedTarget::Count(5)
    );
    let source = BorrowedSource::Text("value".to_owned());
    assert_eq!(BorrowedTarget::from(&source), BorrowedTarget::Text("value"));
}

#[kithara::test(native, flash(false))]
fn mirrors_into_an_explicitly_renamed_variant() {
    assert!(matches!(
        ExternalName::from(LocalName::Local),
        ExternalName::Renamed
    ));
}

#[kithara::test(native, flash(false))]
fn mirrors_into_a_struct_without_its_skipped_fields() {
    let wide = Wide {
        kept: 2,
        dropped: 9,
    };
    assert_eq!(wide.dropped, 9);
    assert_eq!(Narrow::from(wide), Narrow { kept: 2 });
}
