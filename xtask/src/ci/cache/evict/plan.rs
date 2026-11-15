use std::collections::HashMap;

use super::entry::Entry;

/// What one listing found of an entry.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Stored {
    pub(super) size: u64,
    /// The latest write of any of its objects.
    pub(super) written_s: u64,
    /// The objects it is stored as, all removed together.
    pub(super) objects: Vec<String>,
}

/// Everything a scope's bucket holds, as one complete listing saw it.
#[derive(Debug, Default)]
pub(super) struct Listing {
    pub(super) entries: HashMap<Entry, Stored>,
    /// Bytes of every object that is not an entry. They count against the
    /// budget and are never evicted.
    pub(super) other_bytes: u64,
}

impl Listing {
    /// Counts one listed object: as part of the entry its hash names, or,
    /// when it is no entry, by its size alone.
    pub(super) fn insert(&mut self, object: &str, size: u64, written_s: u64) {
        let Some(entry) = Entry::parse(object) else {
            self.other_bytes = self.other_bytes.saturating_add(size);
            return;
        };
        let stored = self.entries.entry(entry).or_insert_with(|| Stored {
            size: 0,
            written_s,
            objects: Vec::new(),
        });
        stored.size = stored.size.saturating_add(size);
        stored.written_s = stored.written_s.max(written_s);
        stored.objects.push(object.to_owned());
    }

    pub(super) fn used(&self) -> u64 {
        self.entries
            .values()
            .map(|stored| stored.size)
            .fold(self.other_bytes, u64::saturating_add)
    }
}

/// An entry chosen for eviction, and what it was chosen by.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Victim {
    pub(super) entry: Entry,
    pub(super) size: u64,
    /// Its last use: the later of its write and its last read.
    pub(super) used_s: u64,
    /// Whether anything read it since it was written.
    pub(super) read: bool,
}

/// The entries to evict, used longest ago first.
///
/// Nothing goes until the bucket holds four fifths of its quota, and then
/// enough goes to bring it down to thirteen twentieths: a margin wide enough
/// that a pass every half hour stays ahead of a day's writes, and narrow
/// enough that what stays is what the fleet still reads.
pub(super) fn victims(listing: &Listing, reads: &HashMap<Entry, u64>, quota: u64) -> Vec<Victim> {
    let mut used = listing.used();
    if used < quota / 5 * 4 {
        return Vec::new();
    }
    let mut candidates = listing
        .entries
        .iter()
        .map(|(&entry, stored)| {
            let read = reads.get(&entry).copied();
            Victim {
                entry,
                size: stored.size,
                used_s: read.map_or(stored.written_s, |read| read.max(stored.written_s)),
                read: read.is_some_and(|read| read > stored.written_s),
            }
        })
        .collect::<Vec<_>>();
    candidates.sort_unstable_by_key(|victim| (victim.used_s, victim.entry));
    let target = quota / 20 * 13;
    let mut chosen = Vec::new();
    for victim in candidates {
        if used <= target {
            break;
        }
        used = used.saturating_sub(victim.size);
        chosen.push(victim);
    }
    chosen
}

/// The victims nothing has read since they were chosen.
pub(super) fn still_unused(victims: &[Victim], reads: &HashMap<Entry, u64>) -> Vec<Victim> {
    victims
        .iter()
        .filter(|victim| {
            reads
                .get(&victim.entry)
                .is_none_or(|&read| read <= victim.used_s)
        })
        .copied()
        .collect()
}

/// Forgets every read the listing already accounts for: a read of an entry
/// that is gone, and a read older than the entry's own write.
pub(super) fn prune(reads: &mut HashMap<Entry, u64>, listing: &Listing) {
    reads.retain(|entry, read| {
        listing
            .entries
            .get(entry)
            .is_some_and(|stored| *read > stored.written_s)
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(byte: u8) -> Entry {
        Entry::from([byte; 32])
    }

    fn listing(entries: &[(u8, u64, u64)], other_bytes: u64) -> Listing {
        Listing {
            entries: entries
                .iter()
                .map(|&(byte, size, written_s)| {
                    (
                        entry(byte),
                        Stored {
                            size,
                            written_s,
                            objects: Vec::new(),
                        },
                    )
                })
                .collect(),
            other_bytes,
        }
    }

    #[test]
    fn a_listed_object_is_an_entry_only_when_sccache_wrote_it() {
        let hash = crate::consts::ENTRY_HASH;
        let mut listing = Listing::default();

        listing.insert(&format!("sccache/a/b/c/{hash}"), 300, 100);
        listing.insert("sccache/.sccache_check", 13, 100);

        assert_eq!(listing.entries.len(), 1);
        assert_eq!(listing.other_bytes, 13);
        assert_eq!(listing.used(), 313);
    }

    #[test]
    fn snapshots_share_the_quota_and_keep_recently_read_outputs() {
        let hash = crate::consts::ENTRY_HASH;
        let snapshot = format!("target-snapshots/{hash}/{hash}.tar");
        let compiler = format!("sccache/a/b/c/{hash}");
        let mut listing = Listing::default();
        listing.insert(&snapshot, 900, 1);
        listing.insert(&compiler, 300, 2);
        let snapshot = Entry::parse(&snapshot).expect("an evictable snapshot");
        let compiler = Entry::parse(&compiler).unwrap();

        assert_eq!(victims(&listing, &HashMap::new(), 1500)[0].entry, snapshot);
        let reads = HashMap::from([(snapshot, 3)]);
        let selected = victims(&listing, &reads, 1500);
        assert_eq!(selected.len(), 1);
        assert_eq!(selected[0].entry, compiler);
        assert!(still_unused(&selected, &HashMap::from([(compiler, 4)])).is_empty());
    }

    #[test]
    fn a_kache_entry_is_both_of_its_objects() {
        let hash = crate::consts::ENTRY_HASH;
        let manifest = format!("kache/v3/manifests/serde/{hash}.json");
        let pack = format!("kache/v3/packs/serde/{hash}.tar.zst");
        let mut listing = Listing::default();

        listing.insert(&manifest, 10, 100);
        listing.insert(&pack, 290, 101);

        let [(_, stored)] = listing.entries.iter().collect::<Vec<_>>()[..] else {
            panic!("one entry: {:?}", listing.entries);
        };
        assert_eq!((stored.size, stored.written_s), (300, 101));
        assert_eq!(stored.objects, [manifest, pack]);
        assert_eq!(listing.used(), 300);
    }

    #[test]
    fn a_bucket_below_four_fifths_of_its_quota_keeps_everything() {
        let listing = listing(&[(1, 300, 100), (2, 300, 200)], 199);

        assert!(victims(&listing, &HashMap::new(), 1000).is_empty());
    }

    /// The age rule this replaces measured a write, so the entries every
    /// build reads - the oldest written - were the first to expire.
    #[test]
    fn eviction_takes_what_was_used_longest_ago_until_thirteen_twentieths_remain() {
        let listing = listing(&[(1, 300, 100), (2, 300, 200), (3, 300, 300)], 100);
        let reads = HashMap::from([(entry(1), 400)]);

        let chosen = victims(&listing, &reads, 1000);

        assert_eq!(
            chosen,
            [
                Victim {
                    entry: entry(2),
                    size: 300,
                    used_s: 200,
                    read: false,
                },
                Victim {
                    entry: entry(3),
                    size: 300,
                    used_s: 300,
                    read: false,
                },
            ]
        );
    }

    /// The startup probe and the snapshot layers fill the budget like any
    /// object, but evicting the probe turns a lane's cache read-only and the
    /// snapshots answer to their own retention.
    #[test]
    fn what_is_not_an_entry_counts_but_is_never_chosen() {
        let listing = listing(&[(1, 100, 100)], 900);

        let chosen = victims(&listing, &HashMap::new(), 1000);

        assert_eq!(
            chosen.iter().map(|victim| victim.entry).collect::<Vec<_>>(),
            [entry(1)]
        );
        assert_eq!(listing.used(), 1000);
    }

    #[test]
    fn a_victim_read_after_it_was_chosen_is_kept() {
        let listing = listing(&[(1, 300, 100), (2, 300, 200), (3, 300, 300)], 100);
        let chosen = victims(&listing, &HashMap::new(), 1000);
        let reads = HashMap::from([(entry(1), 500)]);

        let still = still_unused(&chosen, &reads);

        assert!(chosen.iter().any(|victim| victim.entry == entry(1)));
        assert!(still.iter().all(|victim| victim.entry != entry(1)));
        assert_eq!(still.len(), chosen.len() - 1);
    }

    #[test]
    fn a_read_the_listing_already_accounts_for_is_forgotten() {
        let listing = listing(&[(1, 1, 100), (2, 1, 200)], 0);
        let mut reads = HashMap::from([(entry(1), 150), (entry(2), 150), (entry(3), 150)]);

        prune(&mut reads, &listing);

        assert_eq!(reads, HashMap::from([(entry(1), 150)]));
    }
}
