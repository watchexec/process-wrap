#![doc(html_favicon_url = "https://watchexec.github.io/logo:command-group.svg")]
#![doc(html_logo_url = "https://watchexec.github.io/logo:command-group.svg")]
#![cfg_attr(docsrs, feature(doc_auto_cfg))]
// #![warn(missing_docs)]

// #[cfg(feature = "std")]
// pub mod std;

#[cfg(feature = "tokio1")]
pub mod tokio;

#[cfg(all(
	windows,
	feature = "job-object",
	any(feature = "std", feature = "tokio1")
))]
mod windows;

/// Internal memoization of the exit status of a child process.
#[allow(dead_code)] // easier than listing exactly which featuresets use it
#[derive(Debug)]
pub(crate) enum ChildExitStatus {
	Running,
	Exited(std::process::ExitStatus),
}
