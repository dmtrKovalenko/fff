bitflags::bitflags! {
    #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
    pub struct Status: u32 {
        const CURRENT = 0;
        const INDEX_NEW = 1 << 0;
        const INDEX_MODIFIED = 1 << 1;
        const INDEX_DELETED = 1 << 2;
        const INDEX_RENAMED = 1 << 3;
        const INDEX_TYPECHANGE = 1 << 4;
        const WT_NEW = 1 << 7;
        const WT_MODIFIED = 1 << 8;
        const WT_DELETED = 1 << 9;
        const WT_TYPECHANGE = 1 << 10;
        const WT_RENAMED = 1 << 11;
        const WT_UNREADABLE = 1 << 12;
        const IGNORED = 1 << 14;
        const CONFLICTED = 1 << 15;
    }
}
