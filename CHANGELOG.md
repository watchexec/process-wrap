# Changelog

All notable changes to this project will be documented in this file.

See [conventional commits](https://www.conventionalcommits.org/) for commit guidelines.

---
## [11.0.0](https://github.com/watchexec/process-wrap/compare/v10.0.2..v11.0.0) - 2026-11-24


- **Deps:** Bump MSRV to 1.88 - ([889a600](https://github.com/watchexec/process-wrap/commit/889a6002ccc8d73674b702f4a98d096a64189267))
- **Deps:** Update the lockfile - ([d6649ec](https://github.com/watchexec/process-wrap/commit/d6649ec296ce51dfc14fee226f8d2e221200c0d3))
- **Deps:** Update all dependencies, bump MSRV to 1.88 (#79) - ([ff2ab66](https://github.com/watchexec/process-wrap/commit/ff2ab66480a8977073c65aa90c3437bcb60e0e47))
- **Deps:** Bump nix from 0.31.3 to 0.32.0 in the cargo group - ([d5c1eb6](https://github.com/watchexec/process-wrap/commit/d5c1eb6ef5fbe4dcf93d42c57d45e3fb8a585d61))
- **Deps:** Bump nix from 0.31.3 to 0.32.0 in the cargo group (#86) - ([eadd4b8](https://github.com/watchexec/process-wrap/commit/eadd4b867774514015fadfe23a48b9542f7e7869))
- **Documentation:** Fix `DETACHED_PROCESS` in the example. (#66) - ([b0e2d0c](https://github.com/watchexec/process-wrap/commit/b0e2d0c437fa520d9b9b1e80b27c0655fdd48fb5))
- **Documentation:** Build crate docs with only one frontend enabled - ([51e3c63](https://github.com/watchexec/process-wrap/commit/51e3c63f65a70edd52e5834c1787a55ebe469d0e))
- **Documentation:** Build crate docs with only one frontend enabled (#78) - ([52648b3](https://github.com/watchexec/process-wrap/commit/52648b3fc3b9b67c1c6aac284f2b7bc9392af9bc))
- **Feature:** Add std PropagateThreadDeath and std KillOnDrop support. (#69) - ([4764ac6](https://github.com/watchexec/process-wrap/commit/4764ac6741f7296d69cc973202ca25dd572c9061))
- **Style:** Rustfmt - ([6148fa3](https://github.com/watchexec/process-wrap/commit/6148fa32ba484d25a1d7ff5a56e2ea0d2d57a55f))
- **Test:** Spawn the same command twice with each Unix wrapper - ([d5faf63](https://github.com/watchexec/process-wrap/commit/d5faf633740e6677b3289aa7e56f72fdd8ac15f2))
- **Test:** Cover untested wrapper behaviour (#83) - ([2f4bc39](https://github.com/watchexec/process-wrap/commit/2f4bc393d8bd03b4b1c81fee2e13ce82dd42906e))

### Creation-flags

- **Documentation:** Fix typo - ([8e475cd](https://github.com/watchexec/process-wrap/commit/8e475cd78f9f038d7c97b5e7a9df291a74ffe737))
- **Test:** Pass flags through unchanged without JobObject - ([55f67c3](https://github.com/watchexec/process-wrap/commit/55f67c34021ade113abfd2e10d723c3f6f7399b0))

### Deps

- **Bugfix:** Drop redundant homepage, make indexmap optional - ([250e189](https://github.com/watchexec/process-wrap/commit/250e189d7479bd6d6aa6f624572acdf2c2105fe7))
- **Bugfix:** Don't enable tracing by default - ([2d88eef](https://github.com/watchexec/process-wrap/commit/2d88eef7f3c2e9e9409f8e9f89c87c2dc1288589))

### Job-object

- **Bugfix:** Wait until every process in the job has exited - ([f460a7f](https://github.com/watchexec/process-wrap/commit/f460a7fe1cb57f945ca4211b410105250886fcfc))
- **Bugfix:** Only count a thread as resumed once it runs - ([f860581](https://github.com/watchexec/process-wrap/commit/f8605818689849eed29df015d8bf4d69e867730c))
- **Bugfix:** Wait until every process in the job has exited (#76) - ([ccb5885](https://github.com/watchexec/process-wrap/commit/ccb58857b0bad6e3d8b85b7c9041bd407aa4cd1b))
- **Refactor:** Only set job limits when there are some - ([1c37063](https://github.com/watchexec/process-wrap/commit/1c37063f1013a92f64305cb7388d4a1852fb28c8))
- **Test:** Wait for descendants that outlive the leader - ([695e2ae](https://github.com/watchexec/process-wrap/commit/695e2ae80812351096c5a512796420bbda93ff73))
- **Test:** Exit the helper before the harness writes to a closed pipe - ([5089b55](https://github.com/watchexec/process-wrap/commit/5089b5551c835d87368bcc8ddae3c1c834815ff8))

### Kill-on-drop

- **Test:** Run the Windows test, and check into_inner disarms it - ([d95b322](https://github.com/watchexec/process-wrap/commit/d95b32217049cebf5bcf50cf6c9a9f72ce34f4d0))
- **Test:** Check the unwrapped child is running through try_wait - ([49a603b](https://github.com/watchexec/process-wrap/commit/49a603bc16def12c8a2db0819dfc382535cd7f39))
- **Test:** Check the dropped child is gone through kill(pid, 0) - ([5d60015](https://github.com/watchexec/process-wrap/commit/5d600150760661b33c06fc375126d6029f3889ed))

### Process-group

- **Test:** Attach to a leader's group, and read its pgid - ([33c0fe5](https://github.com/watchexec/process-wrap/commit/33c0fe5c20a8f2bbc97d3faa783a1eaca27b2da0))

### Process-session

- **Bugfix:** Spawn the same command more than once - ([a64e7a9](https://github.com/watchexec/process-wrap/commit/a64e7a911045f792853ce0d0da92aa62bf0e7818))
- **Bugfix:** Spawn the same command more than once (#80) - ([97e4c6d](https://github.com/watchexec/process-wrap/commit/97e4c6d96c85bfa1c14bd7ff815c43d14944c548))

### Propagate-thread-death

- **Bugfix:** Install one hook per command - ([c4fe68d](https://github.com/watchexec/process-wrap/commit/c4fe68dbd2c95f5283cba6876f3cb27495af5b6b))
- **Documentation:** Describe the Tokio thread semantics, fix prctl typo - ([f6e9a26](https://github.com/watchexec/process-wrap/commit/f6e9a266af3da1b09b481727ca0f1c6f3851d0db))
- **Documentation:** Point to the std wrapper for control over the thread - ([a1227d3](https://github.com/watchexec/process-wrap/commit/a1227d389f8d44f336a5459716dbb06ed2b7ee6c))
- **Documentation:** Correct which thread a Tokio child is tied to - ([b6b4ff9](https://github.com/watchexec/process-wrap/commit/b6b4ff98ed2cff75ab3bb309c42ff1d239d7da58))
- **Feature:** Take a plain signal number in new() - ([b499629](https://github.com/watchexec/process-wrap/commit/b499629625aba1a2bb24255262af59cb2f844fb8))
- **Refactor:** Move the pre_exec hook to a shared module - ([3083dd4](https://github.com/watchexec/process-wrap/commit/3083dd46dc3b2c4c93fd007ce86460556c40710e))
- **Test:** Reuse a command after its spawning thread exits - ([a6fccc1](https://github.com/watchexec/process-wrap/commit/a6fccc163f6b58a60be6b373c8e82c496c6b8002))
- **Test:** Deliver a custom death signal - ([59b585a](https://github.com/watchexec/process-wrap/commit/59b585ac7c78a0ee09ecfdac1f770ad1a534a06c))

### Reset-sigmask

- **Test:** Check the child's blocked signals are cleared - ([370de6a](https://github.com/watchexec/process-wrap/commit/370de6a9be6e3bffdf5753b9c64cd7c0ea14cc9c))

### Std

- **Test:** Wait_with_output with stderr, and with both streams beyond a pipe's buffer - ([3b2909e](https://github.com/watchexec/process-wrap/commit/3b2909e24f057b46e6837ad7483961549585e46b))

### Tokio

- **Feature:** Add PropagateThreadDeath - ([69b0dc3](https://github.com/watchexec/process-wrap/commit/69b0dc3e20afb471438f7d7e01b95316edbb8d4f))
- **Feature:** Add PropagateThreadDeath (#77) - ([e8647dd](https://github.com/watchexec/process-wrap/commit/e8647dd8f12c0eae424cac797bbc39a57b03bb37))

### Windows

- **Feature:** Add shareable job handles to JobObjectChild - ([cd75359](https://github.com/watchexec/process-wrap/commit/cd75359ecdab0f6a788da4792ac5bea65765f49c))
- **Feature:** Add shareable job handles to JobObjectChild (#68) - ([33dd928](https://github.com/watchexec/process-wrap/commit/33dd9288cdd5c3d2b4f024bfad1ebc10c156975c))

---
## [10.0.1](https://github.com/watchexec/process-wrap/compare/v10.0.0..v10.0.1) - 2026-09-23


- **Bugfix:** Avoid tracing after fork - ([a32bc17](https://github.com/watchexec/process-wrap/commit/a32bc176da9bf3c9acef1524061ee4581e31adff))
- **Bugfix:** Stop capturing unused signal mask - ([5143195](https://github.com/watchexec/process-wrap/commit/5143195d9bb397633b3f7f0c8fcf206c395fdb17))

---
## [10.0.0](https://github.com/watchexec/process-wrap/compare/v9.1.0..v10.0.0) - 2026-08-24


- **Bugfix:** Correct wrapper downcasting - ([ed642af](https://github.com/watchexec/process-wrap/commit/ed642af2bbc6ffbc3c7c2822cd6a98203baebe7c))
- **Bugfix:** Keep wrapper registry live during hooks - ([ad93dd5](https://github.com/watchexec/process-wrap/commit/ad93dd5f94d65b940f0451940f8da0da226ebaf3))
- **Bugfix:**  [**⚠️ breaking ⚠️**] Make stored wrapper operations type-safe (#37) - ([ee6bc07](https://github.com/watchexec/process-wrap/commit/ee6bc07e480e5560afff2175089fc408b6881b43))
- **Bugfix:** Preserve live wrapper composition (#38) - ([44fa448](https://github.com/watchexec/process-wrap/commit/44fa4481c5ee6cae10ddae5b3322891d3d21b07c))
- **Bugfix:** Expose direct process-group children - ([8e1ed59](https://github.com/watchexec/process-wrap/commit/8e1ed59b6557a4965911d4fbc50d2979537c412b))
- **Documentation:** Describe typed wrapper extension - ([d766a6e](https://github.com/watchexec/process-wrap/commit/d766a6e836f49a46bf61c214ceaec5e545c0e39c))
- **Documentation:** Explain order-independent job composition - ([8fc6e1f](https://github.com/watchexec/process-wrap/commit/8fc6e1f447cc026239b5ce0ed50968f808b5a6eb))
- **Documentation:** Describe typed wrapper extension - ([08ca811](https://github.com/watchexec/process-wrap/commit/08ca811180d929d5b68e12b39fd00a332817fa0c))
- **Documentation:** Correct extension and release-link guidance - ([3d1f62e](https://github.com/watchexec/process-wrap/commit/3d1f62e6fd5d02d904030c67c6d459ec6fe063fe))
- **Documentation:** Restore child-layer motivation - ([5793f06](https://github.com/watchexec/process-wrap/commit/5793f0680ea1bb080c9c5ffefd742342d23035da))
- **Feature:**  [**⚠️ breaking ⚠️**] Make wrapper extension typed - ([a3fcaf2](https://github.com/watchexec/process-wrap/commit/a3fcaf26fb85ad3dfb5f8f2d5ae5ac1a10dc1edb))
- **Feature:** Add native child traversal capabilities - ([a1b8dbf](https://github.com/watchexec/process-wrap/commit/a1b8dbfadc77b22500069410338f98af5d390019))
- **Feature:** Replace panicking accessors with fallible ones (#40) - ([9f1f844](https://github.com/watchexec/process-wrap/commit/9f1f844dc3e0eab16e4bb0d28e460e4810d04627))
- **Feature:** Add boxed-child spawning - ([c9e536d](https://github.com/watchexec/process-wrap/commit/c9e536da3db4e6d615d1397cd2acefd134165136))
- **Feature:** Add spawn_with_child (#41) - ([2f4fb3d](https://github.com/watchexec/process-wrap/commit/2f4fb3d636c7b24a1fef56adf3f768be79d0892d))
- **Refactor:** State assertion failures clearly - ([ae6b10c](https://github.com/watchexec/process-wrap/commit/ae6b10ce99c84129da974605420816d19dcf048b))
- **Refactor:**  [**⚠️ breaking ⚠️**] Remove panicking child accessors - ([28b6a7d](https://github.com/watchexec/process-wrap/commit/28b6a7dd855d372cd6a84b3583abb66aad9c2206))
- **Test:** Cover concrete wrapper lookup - ([f353846](https://github.com/watchexec/process-wrap/commit/f3538468f5fa6b4eba2d2f0671ca1eb4bbcf2e8e))
- **Test:** Gate wrappers by their required features - ([5542e7b](https://github.com/watchexec/process-wrap/commit/5542e7b31e9ebdae5c643f01e2714c72b8a7c76c))
- **Test:** Cover child wrapper capabilities - ([5382944](https://github.com/watchexec/process-wrap/commit/538294475ff751e7921dfeddab6f46aec75a2846))
- **Test:** Cover boxed-child spawning lifecycles - ([b65bcdc](https://github.com/watchexec/process-wrap/commit/b65bcdcfb3acbb977cb00078e86632a917aad6b4))

### Windows

- **Bugfix:** Preserve creation flags with job objects - ([be83bbb](https://github.com/watchexec/process-wrap/commit/be83bbb2bfb79816d85acc12329a4f33130fafd7))
- **Feature:** Assign jobs through child process handles - ([800e17f](https://github.com/watchexec/process-wrap/commit/800e17fb5e1dab833c1d68821bf71dfb84b606f8))
- **Test:** Poll for process exit - ([1e7f3ff](https://github.com/watchexec/process-wrap/commit/1e7f3ffeeaee41bfeea980a0aae1f62e33aa8f7e))
- **Test:** Avoid PowerShell process lifetime flake - ([ec41a70](https://github.com/watchexec/process-wrap/commit/ec41a70abd0a3a0e7ae30747f0af6345b0aaa112))
- **Test:** Make descendant readiness deterministic - ([49b4bca](https://github.com/watchexec/process-wrap/commit/49b4bcab1dcc7ce3cd52e9d393496add36b69e74))
- **Test:** Invoke descendant helpers by filter - ([d92beb7](https://github.com/watchexec/process-wrap/commit/d92beb7d86bf06f42fe18dceccab152c273caa30))

---
## [9.1.0](https://github.com/watchexec/process-wrap/compare/v9.0.3..9.1.0) - 2026-03-08


- **Feature:** Add `CommandWrap::spawn_with` for custom spawner functions - ([9632e9d](https://github.com/watchexec/process-wrap/commit/9632e9de383f8fe0497612a9f0f2c92a8e01b3ea))

---
## [9.0.3](https://github.com/watchexec/process-wrap/compare/v9.0.2..9.0.3) - 2026-02-12


- **Bugfix:** Docs.rs build - ([5d19d8a](https://github.com/watchexec/process-wrap/commit/5d19d8a6f965c41f86abcdd9710025111142262b))
- **Deps:** Update nix 0.31 - ([2e613c6](https://github.com/watchexec/process-wrap/commit/2e613c6697cf64415de1389e81879f6578589608))
- **Documentation:** Update readme - ([b1422d7](https://github.com/watchexec/process-wrap/commit/b1422d75b148ec5b211a8ce78faf45a83c42a9c0))

---
## [9.0.2](https://github.com/watchexec/process-wrap/compare/v9.0.1..9.0.2) - 2026-02-11


- **Documentation:** Fix incorrect `JobObject::new()` usage in README (#29) - ([f7dd863](https://github.com/watchexec/process-wrap/commit/f7dd863ec6d7ba2822d1d8a92b257707860c719e))

---
## [9.0.1](https://github.com/watchexec/process-wrap/compare/v9.0.0..9.0.1) - 2026-01-20


- **Bugfix:** Restore Sync support - ([9a8bbb9](https://github.com/watchexec/process-wrap/commit/9a8bbb92b334539c29cea480342354a9857c90db))
- **Deps:** Upgrade windows and transitives - ([bbaaef0](https://github.com/watchexec/process-wrap/commit/bbaaef011177f048e79d4fb6fae48c1f72865a23))
- **Documentation:** Better document msrv policy - ([446028b](https://github.com/watchexec/process-wrap/commit/446028b646dc0747aad68754b341a7a8871f3c4f))

---
## [9.0.0](https://github.com/watchexec/process-wrap/compare/v8.2.1..9.0.0) - 2025-08-09


- **Bugfix:** Restore ability to compile without tracing - ([e760d2d](https://github.com/watchexec/process-wrap/commit/e760d2d6e859c795fbe892cc87c85daeb4d4c74a))
- **Documentation:** Add workaround for jobobject ordering - ([6afbf8d](https://github.com/watchexec/process-wrap/commit/6afbf8dfa247f66ea5ade513e6b8822e9695da2c))
- **Feature:** Change child wrapper inner()s to return one layer down (#23) - ([bcfd7fd](https://github.com/watchexec/process-wrap/commit/bcfd7fd889fe957d5921299380ea323a794205a3))
- **Feature:** Add `&mut Command` to `post_spawn()` (#24) - ([93b5dd2](https://github.com/watchexec/process-wrap/commit/93b5dd214c344469deede02db7710746b36f178c))
- **Refactor:** Rename types to remove std/tokio name differentiation (#26) - ([f1cf62e](https://github.com/watchexec/process-wrap/commit/f1cf62e56c8870bdf7c9af620d4261ffafe015aa))

---
## [8.2.1](https://github.com/watchexec/process-wrap/compare/v8.2.0..8.2.1) - 2025-05-15


- **Deps:** Update nix to 0.30 (#18) - ([fa80dc6](https://github.com/watchexec/process-wrap/commit/fa80dc61f8cdedaf537242100b0e48737ff872c7))
- **Deps:** Update lockfile - ([854f074](https://github.com/watchexec/process-wrap/commit/854f0742e31653ead84ffa57623b283dcbff92f1))
- **Documentation:** Mention ordering requirement - ([573f2ba](https://github.com/watchexec/process-wrap/commit/573f2ba98d48134793b24f522196f0d7abf4d87d))

---
## [8.2.0](https://github.com/watchexec/process-wrap/compare/v8.1.0..8.2.0) - 2025-01-12


- **Feature:** Add downcasting feature which adds Any trait bound to ChildWrappers - ([3e48bf8](https://github.com/watchexec/process-wrap/commit/3e48bf86e1bff9c21f8f8cbd68ef77537e3ccf2e))

---
## [8.1.0](https://github.com/watchexec/process-wrap/compare/v8.0.2..8.1.0) - 2025-01-12


- **Bugfix:** Cfg attribute scope (#10) - ([04a41f3](https://github.com/watchexec/process-wrap/commit/04a41f3c3ce1c632e5854ddfbe2fe9e0c5c7101f))
- **Deps:** Push tokio requirement to 1.38 to be able to use stable process_group() - ([ff834da](https://github.com/watchexec/process-wrap/commit/ff834da00187d294a6f779f0dc5d28363acb58df))
- **Deps:** Update lockfile - ([d8c3e1f](https://github.com/watchexec/process-wrap/commit/d8c3e1fa8d52e9df1e7b715c52ff44fc37f80b56))
- **Deps:** Upgrade nix to 0.29 - ([067ff80](https://github.com/watchexec/process-wrap/commit/067ff80779296b2aec047cab3c40d9570197901d))
- **Deps:** Upgrade windows to 0.59 - ([067ff80](https://github.com/watchexec/process-wrap/commit/067ff80779296b2aec047cab3c40d9570197901d))
- **Deps:** Bump MSRV to 1.77 - ([6ec3a36](https://github.com/watchexec/process-wrap/commit/6ec3a3631f93b98d2e95d7a4aba5ab492e3a82a2))
- **Documentation:** Add notes to indicate which deps can be bumped safely - ([067ff80](https://github.com/watchexec/process-wrap/commit/067ff80779296b2aec047cab3c40d9570197901d))
- **Feature:** Add try_clone to the child wrapper traits - ([3162a4a](https://github.com/watchexec/process-wrap/commit/3162a4abfbe3440037540bca858b8f912fb419c8))
- **Feature:** Use process_group() from std (available since Rust 1.64) - ([76de1d5](https://github.com/watchexec/process-wrap/commit/76de1d5118b04dcd7a7046e3592880e38d6a9672))
- **Tweak:** Wrap some windows handles that are safe to Send - ([657d4e3](https://github.com/watchexec/process-wrap/commit/657d4e302a399836b092452da588afb54486a1e7))

---
## [8.0.2](https://github.com/watchexec/process-wrap/compare/v8.0.1..8.0.2) - 2024-05-31


- **Deps:** Add tracing feature to remove tracing dep if wanted - ([9f64922](https://github.com/watchexec/process-wrap/commit/9f649226dd409f951a0f985cf2717cbe489deba6))

---
## [8.0.1](https://github.com/watchexec/process-wrap/compare/v8.0.0..8.0.1) - 2024-05-31


- **Deps:** Turn futures into an optional dependency (#7) - ([0c8b54b](https://github.com/watchexec/process-wrap/commit/0c8b54b0a1fcc56a63bc44ba0bebb5c1da3022f9))

---
## [8.0.0](https://github.com/watchexec/process-wrap/compare/v7.1.0..8.0.0) - 2024-04-20


- **API change:** Add explicit Send + Sync bounds - ([6580a62](https://github.com/watchexec/process-wrap/commit/6580a62ce7e67a9c24a4ff7670573865de850737))

---
## [7.1.0](https://github.com/watchexec/process-wrap/compare/v7.0.1..7.1.0) - 2024-04-20


- **Feature:** Add Clone and Copy derives where possible - ([bfd45e0](https://github.com/watchexec/process-wrap/commit/bfd45e0e400551bcd749e76149961ccb56e532fc))
- **Feature:** Add reset-sigmask wrapper - ([19d27d6](https://github.com/watchexec/process-wrap/commit/19d27d630cf136bb5d24f08974f736429677cdc8))

---
## [7.0.1](https://github.com/watchexec/process-wrap/compare/v7.0.0..7.0.1) - 2024-04-16


- **API change:** Remove re-export of nix Signal - ([a1066a7](https://github.com/watchexec/process-wrap/commit/a1066a795fe3279d7e43071ffc536082041fb16a))
- **Documentation:** Yank 7.0.0 - ([f7bbb36](https://github.com/watchexec/process-wrap/commit/f7bbb36c77618a91947fa84029cd47e7241e6fd7))
- **Documentation:** Fix doctests - ([191b8d6](https://github.com/watchexec/process-wrap/commit/191b8d61a51acbd43339aeae2f60aeb278ccef15))

---
## ~~[7.0.0](https://github.com/watchexec/process-wrap/compare/v6.0.1..7.0.0) - 2024-04-16~~ yanked


- **API change:** Remove nix types from public API - ([7de6ac0](https://github.com/watchexec/process-wrap/commit/7de6ac0a3d331d471e6cf09f0d565547cc49708f))
- **Deps:** Bump the cargo group with 1 update - ([d542c6e](https://github.com/watchexec/process-wrap/commit/d542c6ed922c8508f6af8366801c59d144330ead))
- **Deps:** Bump the cargo group with 1 update (#4) - ([fa44e91](https://github.com/watchexec/process-wrap/commit/fa44e91d0f8ba8d463812896a634a2380f4ef7f9))
- **Deps:** Upgrade windows to 0.56 - ([7f2a280](https://github.com/watchexec/process-wrap/commit/7f2a28098bb3cf028609d59a24ebd7f91a45e22c))
- **Documentation:** Fix progress->process in repo name in changelog - ([e8fb3a1](https://github.com/watchexec/process-wrap/commit/e8fb3a1ce81587f809a6773ce17297f1935e42f8))
- **Feature:** Add underlying Command accessors to *CommandWrap - ([2e2f548](https://github.com/watchexec/process-wrap/commit/2e2f548083cf2fe895c4cb03648138a89c569e2a))
- **Feature:** Add pgid accessor to ProcessGroupChild - ([2e2f548](https://github.com/watchexec/process-wrap/commit/2e2f548083cf2fe895c4cb03648138a89c569e2a))
- **Refactor:** Don't require nix::Pid on the public API - ([fd5f5e3](https://github.com/watchexec/process-wrap/commit/fd5f5e32fb9ae902e6cec2d18fe74f54b05c6dac))
- **Test:** Multiprocess behaviour - ([bbe9eed](https://github.com/watchexec/process-wrap/commit/bbe9eede1b6bb6fb522bcc80ebd21210f42c0882))
- **Test:** Multiproc tests for linux specifically - ([bd268d2](https://github.com/watchexec/process-wrap/commit/bd268d228cf0083decce6c72ff13a7ed60b74d4d))
- **Test:** Multiproc for std - ([35270b8](https://github.com/watchexec/process-wrap/commit/35270b82b9f1e5b5aa569103cfafd07ed85e9f74))

---
## [6.0.1](https://github.com/watchexec/process-wrap/compare/v6.0.0..6.0.1) - 2024-03-11


- **Bugfix:** Std doesn't have kill-on-drop - ([5eaef93](https://github.com/watchexec/process-wrap/commit/5eaef93d770ebd2c2307347f1d9a35b25a1dc2c1))
- **Bugfix:** Enable kill-on-drop (tokio only) by default - ([5eaef93](https://github.com/watchexec/process-wrap/commit/5eaef93d770ebd2c2307347f1d9a35b25a1dc2c1))
- **Documentation:** Document that kill-on-drop is Tokio-only - ([7437914](https://github.com/watchexec/process-wrap/commit/74379146c327d8ff68ac64ce224ec0c213af34c0))
- **Documentation:** Fix changelog style for 6.0.0 - ([fc158c9](https://github.com/watchexec/process-wrap/commit/fc158c90ece5ecb47f4fe014e02085d80302f660))

---
## [6.0.0](https://github.com/watchexec/process-wrap/compare/v5.0.1..6.0.0) - 2024-03-11


- **Deps:** Upgrade nix to 0.28 and windows to 0.54 - ([0d24385](https://github.com/watchexec/process-wrap/commit/0d243853cf99324a46f32cc6f08a2a6b27c9b91d))
- **Documentation:** Enable doc_auto_cfg for docsrs - ([d143b09](https://github.com/watchexec/process-wrap/commit/d143b090207608a7ec1c93df125bb096a15d2e8a))
- **Documentation:** Correct COPYRIGHT and CITATION.cff for new name - ([a04124f](https://github.com/watchexec/process-wrap/commit/a04124f0597a41ee01c63634592a971b955e659d))
- **Feature:** Instrument internals (with tracing) - ([5ee79a7](https://github.com/watchexec/process-wrap/commit/5ee79a722efcdda10776cf2c70563cc4b00cc33b))
- **Repo:** Do versions with cargo-release and git-cliff - ([869dbf8](https://github.com/watchexec/process-wrap/commit/869dbf8477f1448fb17738bf6e46785f2e8b1044))
- **Repo:** Add dependabot config - ([5a881b2](https://github.com/watchexec/process-wrap/commit/5a881b2b87ec3752f221be5c46146432b3ced3e8))
- **Repo**: Rename to `process-wrap` and rearchitect.
- **Repo**: Require Rust 1.75 for async trait in trait.
- **Tweak:** Restore Signal re-export on unix - ([fe62ff2](https://github.com/watchexec/process-wrap/commit/fe62ff22bf24a079569a081d34f7c60e068d6e54))
- **Tweak**: Windows: setting `CREATE_SUSPENDED` leaves the process suspended after spawn.

## v5.0.1 (2023-11-18)

- Use [std's `process_group()`](doc.rust-lang.org/std/os/unix/process/trait.CommandExt.html#tymethod.process_group) ([#16](https://github.com/watchexec/command-group/issues/16)).

## v5.0.0 (2023-11-18)

- Change `UnixChildExt::signal` to take `&self` instead of `&mut self`.
- Grouped child `wait`s using upstream `::wait` and `::try_wait` in addition to the internal pgid-based logic, to help with cancellation.
- Optimisations in `tokio::Child::wait()`. ([#25](https://github.com/watchexec/command-group/issues/25), [#26](https://github.com/watchexec/command-group/issues/26))

## v4.1.0 (2023-11-05)

- Add `ErasedChild::id()` method.

## v4.0.0 (2023-11-05)

- Clarify why and in which situations `AsyncGroupChild::wait` may not behave as expected when cancelled.
- Add `AsyncGroupChild::start_kill` to align with Tokio's `Child::start_kill`.
- Change `AsyncGroupChild::kill` to also `wait()` on the child, to align with Tokio's `Child::kill`.
- Add `ErasedChild` abstraction to allow using the same type for grouped and ungrouped children.

## v3.0.0 (2023-10-30)

- Update `nix` to 0.27.
- Increase MSRV to 1.68 (within policy).
- Add note to documentation for Tokio `Child::wait` wrt cancel safety. ([#21](https://github.com/watchexec/command-group/issues/21))

## v2.1.1 (2023-10-30)

(Same as 3.0.0, but yanked due to breakage.)

## v2.1.0 (2023-03-04)

- Add new `.group()` builder API to allow setting Windows flags and use `kill_on_drop`. ([#15](https://github.com/watchexec/command-group/issues/15), [#17](https://github.com/watchexec/command-group/issues/17), [#18](https://github.com/watchexec/command-group/issues/18))

## v2.0.1 (2022-12-28)

- Fix bug on Windows where the wrong pointer was being null checked, leading to timeout errors. ([#13](https://github.com/watchexec/command-group/pull/13))

## v2.0.0 (2022-12-04)

- Increase MSRV to 1.60.0 and change policy for increasing it (no longer a breaking change).
- Wait for all processes in the process group, avoiding zombies. ([#7](https://github.com/watchexec/command-group/pull/7))
- Update `nix` to 0.26 and limit features. ([#8](https://github.com/watchexec/command-group/pull/8))

## v1.0.8 (2021-10-16)

- Bugfix: compiling would fail when Tokio was missing the `io-util` feature (not `io-std`).

## v1.0.7 (2021-10-16) (yanked)

- Bugfix: compiling would fail when Tokio was missing the `io-std` feature.

## v1.0.6 (2021-08-26)

- Correctly handle timeouts on Windows. ([#2](https://github.com/watchexec/command-group/issues/2), [#3](https://github.com/watchexec/command-group/pull/3))

## v1.0.5 (2021-08-13)

- Internal: change usage of `feature = "tokio"` to `feature = "with-tokio"`.
- Documentation: remove wrong mention of blocking reads on `AsyncGroupChild::wait_with_output()`.

## v1.0.4 (2021-07-26)

New: Tokio implementation, gated on the `with-tokio` feature.

## v1.0.3 (2021-07-21)

Bugfix: `GroupChild::try_wait()` would error if called after a child exited by itself.

## v1.0.2 (2021-07-21)

Bugfix: `GroupChild::try_wait()` and `::wait()` could not be called twice.

## v1.0.1 (2021-07-21)

Implement `Send`+`Sync` on `GroupChild` on Windows, and add a `Drop` implementation to close handles
too (whoops). Do our best when `.into_inner()` is used...

## v1.0.0 (2021-07-20)

Initial release
