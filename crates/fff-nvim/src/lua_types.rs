use fff::file_picker::FilePicker;
use fff::git::format_git_status;
use fff::{
    DirItem, DirSearchResult, FileItem, GrepResult, Location, MixedItemRef, MixedSearchResult,
    Score, SearchResult,
};
use mlua::prelude::*;

pub struct SearchResultLua<'a> {
    inner: SearchResult<'a>,
    picker: &'a FilePicker,
}

impl<'a> SearchResultLua<'a> {
    pub fn new(inner: SearchResult<'a>, picker: &'a FilePicker) -> Self {
        Self { inner, picker }
    }
}

pub struct GrepResultLua<'a> {
    inner: GrepResult<'a>,
    picker: &'a FilePicker,
}

impl<'a> GrepResultLua<'a> {
    pub fn new(inner: GrepResult<'a>, picker: &'a FilePicker) -> Self {
        Self { inner, picker }
    }
}

pub struct DirSearchResultLua<'a> {
    inner: DirSearchResult<'a>,
    picker: &'a FilePicker,
}

impl<'a> DirSearchResultLua<'a> {
    pub fn new(inner: DirSearchResult<'a>, picker: &'a FilePicker) -> Self {
        Self { inner, picker }
    }
}

pub struct MixedSearchResultLua<'a> {
    inner: MixedSearchResult<'a>,
    picker: &'a FilePicker,
}

impl<'a> MixedSearchResultLua<'a> {
    pub fn new(inner: MixedSearchResult<'a>, picker: &'a FilePicker) -> Self {
        Self { inner, picker }
    }
}

struct LuaPosition((i32, i32));

impl IntoLua for LuaPosition {
    fn into_lua(self, lua: &Lua) -> LuaResult<LuaValue> {
        let table = lua.create_table()?;
        table.set("line", self.0.0)?;
        table.set("col", self.0.1)?;
        Ok(LuaValue::Table(table))
    }
}

fn file_item_into_lua(item: &FileItem, lua: &Lua, picker: &FilePicker) -> LuaResult<LuaValue> {
    let table = lua.create_table()?;
    table.set("type", "file")?;
    table.set("relative_path", item.relative_path(picker))?;
    table.set("name", item.file_name(picker))?;
    table.set("size", item.size)?;
    table.set("modified", item.modified)?;
    table.set("access_frecency_score", item.access_frecency_score)?;
    table.set(
        "modification_frecency_score",
        item.modification_frecency_score,
    )?;
    table.set("total_frecency_score", item.total_frecency_score())?;
    table.set("git_status", format_git_status(item.git_status))?;
    table.set("git_recency_score", item.git_recency_score)?;
    table.set("is_binary", item.is_binary())?;
    Ok(LuaValue::Table(table))
}

fn dir_item_into_lua(item: &DirItem, lua: &Lua, picker: &FilePicker) -> LuaResult<LuaValue> {
    let table = lua.create_table()?;
    let name = item
        .dir_name(picker)
        .trim_end_matches(std::path::MAIN_SEPARATOR)
        .trim_end_matches('/')
        .to_owned();
    table.set("type", "directory")?;
    table.set("relative_path", item.relative_path(picker))?;
    table.set("name", name)?;
    table.set("max_access_frecency", item.max_access_frecency())?;
    Ok(LuaValue::Table(table))
}

fn score_into_lua(score: &Score, lua: &Lua) -> LuaResult<LuaValue> {
    let table = lua.create_table()?;
    table.set("total", score.total)?;
    table.set("base_score", score.base_score)?;
    table.set("filename_bonus", score.filename_bonus)?;
    table.set("special_filename_bonus", score.special_filename_bonus)?;
    table.set("frecency_boost", score.frecency_boost)?;
    table.set("git_status_boost", score.git_status_boost)?;
    table.set("git_recency_boost", score.git_recency_boost)?;
    table.set("distance_penalty", score.distance_penalty)?;
    table.set("current_file_penalty", score.current_file_penalty)?;
    table.set("combo_match_boost", score.combo_match_boost)?;
    table.set("path_alignment_bonus", score.path_alignment_bonus)?;
    table.set("match_type", score.match_type)?;
    table.set("exact_match", score.exact_match)?;
    Ok(LuaValue::Table(table))
}

fn set_match_ranges(lua: &Lua, item: &LuaTable, ranges: &[(u32, u32)]) -> LuaResult<()> {
    let ranges_table = lua.create_table()?;

    for (i, &(start, end)) in ranges.iter().enumerate() {
        let range = lua.create_table()?;
        range.set(1, start)?;
        range.set(2, end)?;
        ranges_table.set(i + 1, range)?;
    }

    item.set("match_ranges", ranges_table)
}

fn location_into_lua(location: &Location, lua: &Lua) -> LuaResult<LuaValue> {
    let table = lua.create_table()?;
    match location {
        Location::Line(line) => {
            table.set("line", *line)?;
        }
        Location::Position { line, col } => {
            table.set("line", *line)?;
            table.set("col", *col)?;
        }
        Location::Range { start, end } => {
            table.set("start", LuaPosition(*start))?;
            table.set("end", LuaPosition(*end))?;
        }
    }
    Ok(LuaValue::Table(table))
}

impl IntoLua for SearchResultLua<'_> {
    fn into_lua(self, lua: &Lua) -> LuaResult<LuaValue> {
        let table = lua.create_table()?;

        // Convert items
        let items_table = lua.create_table()?;
        for (i, item) in self.inner.items.iter().enumerate() {
            let lua_item = file_item_into_lua(item, lua, self.picker)?;
            if let LuaValue::Table(item_table) = &lua_item
                && let Some(ranges) = self.inner.match_byte_offsets.get(i)
            {
                set_match_ranges(lua, item_table, ranges.as_slice())?;
            }
            items_table.set(i + 1, lua_item)?;
        }
        table.set("items", items_table)?;

        // Convert scores
        let scores_table = lua.create_table()?;
        for (i, score) in self.inner.scores.iter().enumerate() {
            scores_table.set(i + 1, score_into_lua(score, lua)?)?;
        }
        table.set("scores", scores_table)?;

        table.set("total_matched", self.inner.total_matched)?;
        table.set("total_files", self.inner.total_files)?;

        if let Some(location) = &self.inner.location {
            table.set("location", location_into_lua(location, lua)?)?;
        }

        Ok(LuaValue::Table(table))
    }
}

impl IntoLua for DirSearchResultLua<'_> {
    fn into_lua(self, lua: &Lua) -> LuaResult<LuaValue> {
        let table = lua.create_table()?;

        let items_table = lua.create_table()?;
        for (i, item) in self.inner.items.iter().enumerate() {
            items_table.set(i + 1, dir_item_into_lua(item, lua, self.picker)?)?;
        }
        table.set("items", items_table)?;

        let scores_table = lua.create_table()?;
        for (i, score) in self.inner.scores.iter().enumerate() {
            scores_table.set(i + 1, score_into_lua(score, lua)?)?;
        }
        table.set("scores", scores_table)?;

        table.set("total_matched", self.inner.total_matched)?;
        table.set("total_dirs", self.inner.total_dirs)?;

        Ok(LuaValue::Table(table))
    }
}

impl IntoLua for MixedSearchResultLua<'_> {
    fn into_lua(self, lua: &Lua) -> LuaResult<LuaValue> {
        let table = lua.create_table()?;

        let items_table = lua.create_table()?;
        for (i, item) in self.inner.items.iter().enumerate() {
            let lua_item = match item {
                MixedItemRef::File(file) => file_item_into_lua(file, lua, self.picker)?,
                MixedItemRef::Dir(dir) => dir_item_into_lua(dir, lua, self.picker)?,
            };
            items_table.set(i + 1, lua_item)?;
        }
        table.set("items", items_table)?;

        let scores_table = lua.create_table()?;
        for (i, score) in self.inner.scores.iter().enumerate() {
            scores_table.set(i + 1, score_into_lua(score, lua)?)?;
        }
        table.set("scores", scores_table)?;

        table.set("total_matched", self.inner.total_matched)?;
        table.set("total_files", self.inner.total_files)?;
        table.set("total_dirs", self.inner.total_dirs)?;

        if let Some(location) = &self.inner.location {
            table.set("location", location_into_lua(location, lua)?)?;
        }

        Ok(LuaValue::Table(table))
    }
}

impl IntoLua for GrepResultLua<'_> {
    fn into_lua(self, lua: &Lua) -> LuaResult<LuaValue> {
        let table = lua.create_table()?;

        // Convert grep match items — each includes file metadata + match metadata
        let items_table = lua.create_table()?;
        for (i, m) in self.inner.matches.iter().enumerate() {
            let item = lua.create_table()?;

            // File metadata from the deduplicated files vec
            let file = self.inner.files[m.file_index];
            item.set("relative_path", file.relative_path(self.picker))?;
            item.set("name", file.file_name(self.picker))?;
            item.set("is_binary", file.is_binary())?;
            item.set("git_status", format_git_status(file.git_status))?;
            item.set("size", file.size)?;
            item.set("modified", file.modified)?;
            item.set("total_frecency_score", file.total_frecency_score())?;
            item.set("access_frecency_score", file.access_frecency_score)?;
            item.set(
                "modification_frecency_score",
                file.modification_frecency_score,
            )?;

            // Match metadata
            item.set("line_number", m.line_number)?;
            item.set("col", m.col)?;
            item.set("byte_offset", m.byte_offset)?;

            // There is a little race window when fff can return matches inside of a non-binary
            // classified entities, the window is minimal but it errors out neovim so guard it
            let is_binary_content = m.line_content.as_bytes().contains(&0u8);
            item.set("is_binary_content", is_binary_content)?;
            item.set("line_content", m.line_content.as_str())?;

            set_match_ranges(lua, &item, m.match_byte_offsets.as_slice())?;

            // Fuzzy match score (only set in fuzzy grep mode, nil otherwise)
            if let Some(score) = m.fuzzy_score {
                item.set("fuzzy_score", score)?;
            }

            items_table.set(i + 1, item)?;
        }
        table.set("items", items_table)?;

        table.set("total_matched", self.inner.matches.len())?;
        table.set("total_files_searched", self.inner.total_files_searched)?;
        table.set("total_files", self.inner.total_files)?;
        table.set("filtered_file_count", self.inner.filtered_file_count)?;
        table.set("next_file_offset", self.inner.next_file_offset)?;

        // Pass regex fallback error to Lua (nil if no error)
        if let Some(ref err) = self.inner.regex_fallback_error {
            table.set("regex_fallback_error", err.as_str())?;
        }

        Ok(LuaValue::Table(table))
    }
}
