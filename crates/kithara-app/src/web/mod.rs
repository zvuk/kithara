//! The browser root: the audio host and its pump on the page's main thread,
//! the engine in a Web Worker on a remote host, the studio on a canvas.

mod page;
mod worker;

pub use page::run;
