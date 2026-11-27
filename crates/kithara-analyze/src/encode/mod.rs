mod binary;
mod document;

pub(crate) use binary::binary;
pub(crate) use document::json;

#[cfg(test)]
mod tests;
