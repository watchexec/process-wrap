#![cfg_attr(
	not(any(feature = "std", feature = "tokio1")),
	allow(unused_macros, unused_imports)
)]

macro_rules! Wrap {
	(
		$backend:ty,
		$command:ty,
		$child:ty,
		$childer:ident,
		$first_child_wrapper:expr,
		$frontend:literal,
		$prepared_owner_contract:ident
	) => {
		trait ErasedCommandWrapper: ::std::fmt::Debug + Send + Sync {
			fn as_command_wrapper(&self) -> &dyn CommandWrapper;
			fn as_command_wrapper_mut(&mut self) -> &mut dyn CommandWrapper;
			fn as_any(&self) -> &dyn ::std::any::Any;
			fn as_any_mut(&mut self) -> &mut dyn ::std::any::Any;
		}

		impl<W: CommandWrapper + 'static> ErasedCommandWrapper for W {
			fn as_command_wrapper(&self) -> &dyn CommandWrapper {
				self
			}

			fn as_command_wrapper_mut(&mut self) -> &mut dyn CommandWrapper {
				self
			}

			fn as_any(&self) -> &dyn ::std::any::Any {
				self
			}

			fn as_any_mut(&mut self) -> &mut dyn ::std::any::Any {
				self
			}
		}

		#[derive(Debug, Default)]
		struct WrapperRegistry {
			wrappers: ::indexmap::IndexMap<
				::std::any::TypeId,
				Option<Box<dyn ErasedCommandWrapper>>,
			>,
		}

		impl crate::command::Backend for $backend {
			type NativeCommand = $command;

			fn new_registry() -> Box<dyn ::std::any::Any + Send + Sync> {
				Box::new(WrapperRegistry::default())
			}
		}

		/// A configurable process command with composable wrappers.
		pub type Command = crate::command::Command<$backend>;

		/// Backwards-compatible name for [`Command`].
		pub type CommandWrap = Command;

		/// The command configuration for one spawn attempt.
		pub type SpawnAttempt = crate::command::SpawnAttempt<$backend>;

		/// A child and armed cleanup transaction returned by a [`SpawnProvider`].
		///
		/// The child must implement the complete contract for this frontend's `ChildWrapper`, including
		/// any platform capabilities required by registered wrappers. The transaction must be fresh,
		/// armed, independently owned from the child chain, and able to undo this specific spawn until
		/// process-wrap commits it. On Windows, that includes every ordinary child-finalization and
		/// cleanup-disarm hook plus JobObject owner validation and non-owner disarming. Process-wrap
		/// preallocates the private return sidecar before commit, installs committed residue before the
		/// sole JobObject owner hook, and performs only infallible private moves after that owner
		/// disarms.
		#[derive(Debug)]
		pub struct ProviderProduct {
			child: Box<dyn $childer>,
			transaction: Box<dyn crate::SpawnTransaction>,
		}

		impl ProviderProduct {
			/// Create a provider product from its child and armed cleanup transaction.
			///
			/// Construct this only after the child has been created successfully. The transaction must own
			/// everything needed to terminate and reap that child and release provider resources if a later
			/// hook, pre-commit child-finalization step, or transaction commit fails or unwinds. A successful
			/// commit must release every rollback-only strong owner and independent liveness resource; the
			/// remaining residue is transferred with the returned child. These panic guarantees do not apply
			/// to `panic=abort`.
			pub fn new(
				child: Box<dyn $childer>,
				transaction: Box<dyn crate::SpawnTransaction>,
			) -> Self {
				Self { child, transaction }
			}

			fn into_parts(
				self,
			) -> (Box<dyn $childer>, Box<dyn crate::SpawnTransaction>) {
				(self.child, self.transaction)
			}
		}

		#[cfg(all(test, windows))]
		struct PreparedStateTestRendezvous {
			reached: ::std::sync::mpsc::Sender<()>,
			release: ::std::sync::mpsc::Receiver<()>,
		}

		#[cfg(all(test, windows))]
		impl PreparedStateTestRendezvous {
			fn wait(self) {
				let _ = self.reached.send(());
				let _ = self.release.recv();
			}
		}

		#[cfg(windows)]
		#[derive(Default)]
		struct PreparedChildAdmission {
			closing: bool,
		}

		#[cfg(windows)]
		struct PreparedChildState {
			// Accessors retain this lock through caller inspection. They take `admission` only long
			// enough to linearize admission immediately before invoking the callback.
			value: ::std::sync::Mutex<Option<Box<dyn ::std::any::Any + Send>>>,
			admission: ::std::sync::Mutex<PreparedChildAdmission>,
			type_id: ::std::any::TypeId,
			#[cfg(test)]
			value_lock_attempt: ::std::sync::Mutex<Option<PreparedStateTestRendezvous>>,
			#[cfg(test)]
			after_closing: ::std::sync::Mutex<Option<PreparedStateTestRendezvous>>,
		}

		#[cfg(windows)]
		impl PreparedChildState {
			fn admitted_value(
				&self,
			) -> Option<
				::std::sync::MutexGuard<'_, Option<Box<dyn ::std::any::Any + Send>>>,
			> {
				#[cfg(test)]
				self.wait_for_value_lock_attempt();
				let value = self
					.value
					.lock()
					.unwrap_or_else(::std::sync::PoisonError::into_inner);
				let admission = self
					.admission
					.lock()
					.unwrap_or_else(::std::sync::PoisonError::into_inner);
				if admission.closing {
					drop(admission);
					drop(value);
					return None;
				}
				// Reading `closing == false` while holding `admission` is the accessor's
				// linearization point. Keep `value` locked so a closer cannot take the box before or
				// during the callback, but release `admission` so closing can be published.
				drop(admission);
				Some(value)
			}

			fn close(&self) {
				{
					let mut admission = self
						.admission
						.lock()
						.unwrap_or_else(::std::sync::PoisonError::into_inner);
					// The first write of `true` while holding `admission` is the revocation,
					// failure-cleanup, or consuming-extraction linearization point. A later accessor
					// must acquire this same mutex before inspecting the value and therefore observes
					// the closed state.
					admission.closing = true;
				}
				#[cfg(test)]
				self.wait_after_closing();
			}

			#[cfg(test)]
			fn wait_for_value_lock_attempt(&self) {
				let rendezvous = self
					.value_lock_attempt
					.lock()
					.unwrap_or_else(::std::sync::PoisonError::into_inner)
					.take();
				if let Some(rendezvous) = rendezvous {
					assert!(
						matches!(
							self.value.try_lock(),
							Err(::std::sync::TryLockError::WouldBlock)
						),
						"the acquisition-attempt probe requires an active value owner"
					);
					rendezvous.wait();
				}
			}

			#[cfg(test)]
			fn wait_after_closing(&self) {
				let rendezvous = self
					.after_closing
					.lock()
					.unwrap_or_else(::std::sync::PoisonError::into_inner)
					.take();
				if let Some(rendezvous) = rendezvous {
					rendezvous.wait();
				}
			}
		}

		#[cfg(windows)]
		struct PreparedChildOwner {
			state: ::std::sync::Arc<PreparedChildState>,
			installed_layer: Option<(::std::any::TypeId, usize)>,
		}

		#[cfg(windows)]
		impl PreparedChildOwner {
			fn new(value: Box<dyn ::std::any::Any + Send>) -> Self {
				let type_id = value.as_ref().type_id();
				Self {
					state: ::std::sync::Arc::new(PreparedChildState {
						value: ::std::sync::Mutex::new(Some(value)),
						admission: ::std::sync::Mutex::new(PreparedChildAdmission::default()),
						type_id,
						#[cfg(test)]
						value_lock_attempt: ::std::sync::Mutex::new(None),
						#[cfg(test)]
						after_closing: ::std::sync::Mutex::new(None),
					}),
					installed_layer: None,
				}
			}

			fn view(&self) -> PreparedChildRef<'_> {
				PreparedChildRef { state: &self.state }
			}

			fn installed_token(&self) -> PreparedChild {
				PreparedChild {
					state: ::std::sync::Arc::downgrade(&self.state),
				}
			}

			fn close_and_take(&self) -> Option<Box<dyn ::std::any::Any + Send>> {
				self.state.close();
				self.state
					.value
					.lock()
					.unwrap_or_else(::std::sync::PoisonError::into_inner)
					.take()
			}

			fn has_exclusive_custody(&self) -> bool {
				::std::sync::Arc::strong_count(&self.state) == 1
			}

			fn has_value(&self) -> bool {
				self.state
					.value
					.lock()
					.unwrap_or_else(::std::sync::PoisonError::into_inner)
					.is_some()
			}

			fn installed_in(&self, layer: (::std::any::TypeId, usize)) -> bool {
				self.installed_layer == Some(layer)
			}
		}

		#[cfg(windows)]
		impl Drop for PreparedChildOwner {
			fn drop(&mut self) {
				// Closing is published through the admission mutex before contending for the value.
				// `close_and_take` releases the value lock before returning, so caller-defined
				// destruction runs without either protocol mutex held.
				let value = self.close_and_take();
				drop(value);
			}
		}

		/// Immutable type metadata for state awaiting child-layer installation.
		///
		/// This view cannot clone process-wrap's custody owner or access the prepared value. Use
		/// [`PreparedChildRef::is`] to select a matching detached layer, declare that layer's empty
		/// typed slot through [`ChildWrapperSlots::with_prepared`], and inspect the value through the
		/// installed [`PreparedChild`] only after process-wrap fills the slot.
		#[cfg(windows)]
		#[doc(hidden)]
		pub struct PreparedChildRef<'a> {
			state: &'a PreparedChildState,
		}

		#[cfg(windows)]
		impl ::std::fmt::Debug for PreparedChildRef<'_> {
			fn fmt(&self, formatter: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
				formatter.debug_struct("PreparedChildRef").finish_non_exhaustive()
			}
		}

		#[cfg(windows)]
		impl PreparedChildRef<'_> {
			/// Report whether the pending state has concrete type `T` without exposing its value.
			pub fn is<T: ::std::any::Any>(&self) -> bool {
				self.state.type_id == ::std::any::TypeId::of::<T>()
			}
		}

		#[doc = concat!(
			"Prepared state installed in a matching child layer by process-wrap.\n\n",
			"This token is deliberately non-owning and not `Clone`, and arbitrary mutable access is ",
			"not public. A layer may inspect its installed value immutably with ",
			"[`PreparedChild::with`] while the returned child retains process-wrap's private owner. ",
			"Moving or retaining this token cannot prolong the prepared value's lifetime. Resource ",
			"types which themselves expose interior mutation or independently clonable native owners ",
			"remain responsible for those capabilities.\n\n",
			"On native Windows success, a private outer sidecar retains the strong prepared storage and ",
			"any disarmed spawn-cleanup handle until child disposal or consuming sidecar removal. It ",
			"delegates the full child contract, exposes the immediate application layer through ",
			"`inner` and `inner_mut`, and consumes that application layer through `into_inner`. Its ",
			"private concrete type is the returned trait object's top-level `Any` identity while ",
			"prepared storage remains installed.\n\n",
			"A detached layer cannot manufacture an additional token:\n\n",
			"```compile_fail,E0308\n",
			"# use process_wrap::", $frontend, "::PreparedChild;\n",
			"fn retain_extra_token(prepared: &PreparedChild) {\n",
			"    let _escaped: PreparedChild = prepared.clone();\n",
			"}\n",
			"```\n\n",
			"Nor can downstream code move fields out through process-wrap's private extraction API:\n\n",
			"```compile_fail\n",
			"# use process_wrap::", $frontend, "::PreparedChild;\n",
			"fn take_guard(prepared: &PreparedChild) {\n",
			"    let _guard = prepared.take_prepared_value::<Option<String>>();\n",
			"}\n",
			"```"
		)]
		#[cfg(windows)]
		#[doc(hidden)]
		pub struct PreparedChild {
			state: ::std::sync::Weak<PreparedChildState>,
		}

		#[cfg(windows)]
		impl ::std::fmt::Debug for PreparedChild {
			fn fmt(&self, formatter: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
				formatter.debug_struct("PreparedChild").finish_non_exhaustive()
			}
		}

		#[cfg(windows)]
		impl PreparedChild {
			/// Inspect installed prepared state immutably while its private owner remains live.
			#[doc(hidden)]
			pub fn with<T: ::std::any::Any, R>(&self, inspect: impl FnOnce(&T) -> R) -> Option<R> {
				#[cfg(test)]
				crate::test_allocator::current_probe().observe_operation();
				let state = self.state.upgrade()?;
				let value = state.admitted_value()?;
				value.as_deref()?.downcast_ref::<T>().map(inspect)
			}

			#[cfg(feature = "job-object")]
			pub(crate) fn with_required<T: ::std::any::Any, R>(
				&self,
				unavailable: impl FnOnce() -> R,
				inspect: impl FnOnce(&T) -> R,
			) -> R {
				#[cfg(test)]
				crate::test_allocator::current_probe().observe_operation();
				let Some(state) = self.state.upgrade() else {
					return unavailable();
				};
				let Some(value) = state.admitted_value() else {
					return unavailable();
				};
				let Some(value) = value.as_deref().and_then(|value| value.downcast_ref::<T>()) else {
					return unavailable();
				};
				inspect(value)
			}

			#[cfg(feature = "job-object")]
			pub(crate) fn take_prepared_value<T: ::std::any::Any + Send>(&self) -> Option<T> {
				let state = self.state.upgrade()?;
				// Consuming extraction permanently closes public access before waiting for an active
				// callback. A type mismatch restores the identical box while the state remains closed.
				state.close();
				let mut slot = state
					.value
					.lock()
					.unwrap_or_else(::std::sync::PoisonError::into_inner);
				let value = slot.take()?;
				match value.downcast::<T>() {
					Ok(value) => Some(*value),
					Err(value) => {
						*slot = Some(value);
						None
					}
				}
			}
		}

		#[cfg(all(test, windows, feature = "job-object"))]
		#[derive(Debug)]
		struct PreparedRaceDrop(::std::sync::Arc<::std::sync::atomic::AtomicUsize>);

		#[cfg(all(test, windows, feature = "job-object"))]
		impl Drop for PreparedRaceDrop {
			fn drop(&mut self) {
				self.0
					.fetch_add(1, ::std::sync::atomic::Ordering::SeqCst);
			}
		}

		#[cfg(all(test, windows, feature = "job-object"))]
		struct PreparedRaceCleanup {
			releases: Vec<::std::sync::mpsc::Sender<()>>,
			workers: Vec<::std::thread::JoinHandle<()>>,
		}

		#[cfg(all(test, windows, feature = "job-object"))]
		impl PreparedRaceCleanup {
			fn new() -> Self {
				Self {
					releases: Vec::new(),
					workers: Vec::new(),
				}
			}

			fn release_all(&self) {
				for release in &self.releases {
					let _ = release.send(());
				}
			}

			fn finish(mut self) {
				self.release_all();
				let mut first_panic = None;
				for worker in ::std::mem::take(&mut self.workers) {
					if let Err(payload) = worker.join() {
						if first_panic.is_none() {
							first_panic = Some(payload);
						} else {
							::std::mem::forget(payload);
						}
					}
				}
				if let Some(payload) = first_panic {
					::std::panic::resume_unwind(payload);
				}
			}
		}

		#[cfg(all(test, windows, feature = "job-object"))]
		impl Drop for PreparedRaceCleanup {
			fn drop(&mut self) {
				self.release_all();
				for worker in ::std::mem::take(&mut self.workers) {
					if let Err(payload) = worker.join() {
						::std::mem::forget(payload);
					}
				}
			}
		}

		#[cfg(all(test, windows, feature = "job-object"))]
		#[derive(Clone, Copy)]
		enum PreparedRaceCloser {
			OwnerDrop,
			ConsumingExtraction,
			FailureCleanup,
		}

		#[cfg(all(test, windows, feature = "job-object"))]
		#[derive(Clone, Copy)]
		enum PreparedLifecyclePath {
			NativeDrop,
			ProviderDrop,
			NativeExtraction,
			ProviderExtraction,
			FailureCleanup,
		}

		#[cfg(all(test, windows, feature = "job-object"))]
		struct PreparedExtractionPhase<'a>(&'a ::std::sync::atomic::AtomicBool);

		#[cfg(all(test, windows, feature = "job-object"))]
		impl<'a> PreparedExtractionPhase<'a> {
			fn enter(active: &'a ::std::sync::atomic::AtomicBool) -> Self {
				assert!(
					active
						.compare_exchange(
							false,
							true,
							::std::sync::atomic::Ordering::SeqCst,
							::std::sync::atomic::Ordering::SeqCst,
						)
						.is_ok(),
					"prepared extraction phase entered more than once"
				);
				Self(active)
			}
		}

		#[cfg(all(test, windows, feature = "job-object"))]
		impl Drop for PreparedExtractionPhase<'_> {
			fn drop(&mut self) {
				self.0
					.store(false, ::std::sync::atomic::Ordering::SeqCst);
			}
		}

		#[cfg(all(test, windows, feature = "job-object"))]
		#[derive(Debug)]
		struct PreparedLifecycleChild {
			prepared: Option<PreparedChild>,
			retained: Option<PreparedRaceDrop>,
			extraction_phase: ::std::sync::Arc<::std::sync::atomic::AtomicBool>,
		}

		#[cfg(all(test, windows, feature = "job-object"))]
		impl $childer for PreparedLifecycleChild {
			fn inner(&self) -> &dyn $childer {
				self
			}

			fn inner_mut(&mut self) -> &mut dyn $childer {
				self
			}

			fn into_inner(self: Box<Self>) -> Box<dyn $childer> {
				self
			}

			fn retain_prepared_after_sidecar_removal_layer(&mut self) {
				let prepared = self
					.prepared
					.as_ref()
					.expect("the extraction fixture retains its prepared token");
				let retained = {
					let _phase = PreparedExtractionPhase::enter(&self.extraction_phase);
					prepared
						.take_prepared_value::<PreparedRaceDrop>()
						.expect("consuming extraction transfers the prepared concrete value")
				};
				self.retained = Some(retained);
			}
		}

		#[cfg(all(test, windows, feature = "job-object"))]
		fn assert_queued_prepared_access_is_rejected(closer: PreparedRaceCloser) {
			const TIMEOUT: ::std::time::Duration = ::std::time::Duration::from_secs(5);

			let drops = ::std::sync::Arc::new(::std::sync::atomic::AtomicUsize::new(0));
			let callback_calls =
				::std::sync::Arc::new(::std::sync::atomic::AtomicUsize::new(0));
			let owner = PreparedChildOwner::new(Box::new(PreparedRaceDrop(
				::std::sync::Arc::clone(&drops),
			)));
			let state = ::std::sync::Arc::clone(&owner.state);
			let token = ::std::sync::Arc::new(owner.installed_token());
			let mut cleanup = PreparedRaceCleanup::new();

			let (active_entered_tx, active_entered_rx) = ::std::sync::mpsc::channel();
			let (active_release_tx, active_release_rx) = ::std::sync::mpsc::channel();
			let (active_result_tx, active_result_rx) = ::std::sync::mpsc::channel();
			cleanup.releases.push(active_release_tx.clone());
			let active_token = ::std::sync::Arc::clone(&token);
			cleanup.workers.push(::std::thread::spawn(move || {
				let completed = active_token.with::<PreparedRaceDrop, _>(|_| {
					let _ = active_entered_tx.send(());
					active_release_rx.recv().is_ok()
				}) == Some(true);
				let _ = active_result_tx.send(completed);
			}));
			active_entered_rx
				.recv_timeout(TIMEOUT)
				.expect("the active accessor acquired the prepared value");

			let (queued_reached_tx, queued_reached_rx) = ::std::sync::mpsc::channel();
			let (queued_release_tx, queued_release_rx) = ::std::sync::mpsc::channel();
			*state
				.value_lock_attempt
				.lock()
				.unwrap_or_else(::std::sync::PoisonError::into_inner) =
				Some(PreparedStateTestRendezvous {
					reached: queued_reached_tx,
					release: queued_release_rx,
				});
			cleanup.releases.push(queued_release_tx.clone());
			let (queued_result_tx, queued_result_rx) = ::std::sync::mpsc::channel();
			let queued_token = ::std::sync::Arc::clone(&token);
			let queued_callback_calls = ::std::sync::Arc::clone(&callback_calls);
			cleanup.workers.push(::std::thread::spawn(move || {
				let accessed = queued_token
					.with::<PreparedRaceDrop, _>(|_| {
						queued_callback_calls
							.fetch_add(1, ::std::sync::atomic::Ordering::SeqCst);
					})
					.is_some();
				let _ = queued_result_tx.send(accessed);
			}));
			queued_reached_rx
				.recv_timeout(TIMEOUT)
				.expect("the queued accessor upgraded before closing");

			let (closing_reached_tx, closing_reached_rx) = ::std::sync::mpsc::channel();
			let (closing_release_tx, closing_release_rx) = ::std::sync::mpsc::channel();
			*state
				.after_closing
				.lock()
				.unwrap_or_else(::std::sync::PoisonError::into_inner) =
				Some(PreparedStateTestRendezvous {
					reached: closing_reached_tx,
					release: closing_release_rx,
				});
			cleanup.releases.push(closing_release_tx.clone());
			queued_release_tx
				.send(())
				.expect("release the queued accessor to contend for the value");
			let (destroyed_tx, destroyed_rx) = ::std::sync::mpsc::channel();
			let extraction_token = ::std::sync::Arc::clone(&token);
			cleanup.workers.push(::std::thread::spawn(move || {
				match closer {
					PreparedRaceCloser::OwnerDrop => drop(owner),
					PreparedRaceCloser::ConsumingExtraction => {
						let extracted = extraction_token
							.take_prepared_value::<PreparedRaceDrop>()
							.expect("consuming extraction retains the prepared concrete type");
						drop(extracted);
						drop(owner);
					}
					PreparedRaceCloser::FailureCleanup => {
						let mut prepared = [Some(owner)];
						crate::command::Command::<$backend>::cleanup_prepared(&mut prepared);
					}
				}
				let _ = destroyed_tx.send(());
			}));
			closing_reached_rx
				.recv_timeout(TIMEOUT)
				.expect("the closing operation published its authoritative state");

			assert_eq!(
				destroyed_rx.try_recv(),
				Err(::std::sync::mpsc::TryRecvError::Empty),
				"closing returned while the active accessor retained the value"
			);
			assert_eq!(
				drops.load(::std::sync::atomic::Ordering::SeqCst),
				0,
				"prepared state dropped during active access"
			);
			active_release_tx
				.send(())
				.expect("release the finite active accessor");
			assert!(
				!queued_result_rx
					.recv_timeout(TIMEOUT)
					.expect("the queued accessor completed before the closer took the value"),
				"the queued accessor invoked its callback after closing"
			);
			assert_eq!(
				callback_calls.load(::std::sync::atomic::Ordering::SeqCst),
				0
			);
			closing_release_tx
				.send(())
				.expect("release closing to take the prepared value");
			destroyed_rx
				.recv_timeout(TIMEOUT)
				.expect("the closing operation completed");
			assert_eq!(drops.load(::std::sync::atomic::Ordering::SeqCst), 1);
			assert!(
				token.with::<PreparedRaceDrop, _>(|_| ()).is_none(),
				"a retained token accessed state after closing"
			);
			assert!(
				active_result_rx
					.recv_timeout(TIMEOUT)
					.expect("the active accessor completed")
			);
			cleanup.finish();
		}

		#[cfg(all(test, windows, feature = "job-object"))]
		fn assert_prepared_lifecycle_path_closes(path: PreparedLifecyclePath) {
			const TIMEOUT: ::std::time::Duration = ::std::time::Duration::from_secs(5);

			let drops = ::std::sync::Arc::new(::std::sync::atomic::AtomicUsize::new(0));
			let owner = PreparedChildOwner::new(Box::new(PreparedRaceDrop(
				::std::sync::Arc::clone(&drops),
			)));
			let state = ::std::sync::Arc::clone(&owner.state);
			let token = owner.installed_token();
			let extracts = matches!(
				path,
				PreparedLifecyclePath::NativeExtraction
					| PreparedLifecyclePath::ProviderExtraction
			);
			let extraction_phase =
				::std::sync::Arc::new(::std::sync::atomic::AtomicBool::new(false));
			let child: Box<dyn $childer> = Box::new(PreparedLifecycleChild {
				prepared: extracts.then(|| owner.installed_token()),
				retained: None,
				extraction_phase: ::std::sync::Arc::clone(&extraction_phase),
			});
			let mut cleanup = PreparedRaceCleanup::new();

			let (closing_reached_tx, closing_reached_rx) = ::std::sync::mpsc::channel();
			let (closing_release_tx, closing_release_rx) = ::std::sync::mpsc::channel();
			*state
				.after_closing
				.lock()
				.unwrap_or_else(::std::sync::PoisonError::into_inner) =
				Some(PreparedStateTestRendezvous {
					reached: closing_reached_tx,
					release: closing_release_rx,
				});
			cleanup.releases.push(closing_release_tx.clone());
			let (finished_tx, finished_rx) = ::std::sync::mpsc::channel();
			cleanup.workers.push(::std::thread::spawn(move || {
				match path {
					PreparedLifecyclePath::NativeDrop => {
						let returned: Box<dyn $childer> =
							PreparedOwnerChild::new(child, vec![Some(owner)]);
						drop(returned);
					}
					PreparedLifecyclePath::ProviderDrop => {
						let returned: Box<dyn $childer> =
							CommittedProviderChild::new(child, vec![Some(owner)]);
						drop(returned);
					}
					PreparedLifecyclePath::NativeExtraction => {
						let returned: Box<dyn $childer> =
							PreparedOwnerChild::new(child, vec![Some(owner)]);
						drop(returned.into_inner());
					}
					PreparedLifecyclePath::ProviderExtraction => {
						let returned: Box<dyn $childer> =
							CommittedProviderChild::new(child, vec![Some(owner)]);
						drop(returned.into_inner());
					}
					PreparedLifecyclePath::FailureCleanup => {
						drop(child);
						let mut prepared = [Some(owner)];
						crate::command::Command::<$backend>::cleanup_prepared(&mut prepared);
					}
				}
				let _ = finished_tx.send(());
			}));

			closing_reached_rx
				.recv_timeout(TIMEOUT)
				.expect("the lifecycle path published prepared closing");
			if extracts {
				assert!(
					extraction_phase.load(::std::sync::atomic::Ordering::SeqCst),
					"consuming extraction must publish closing inside the prepared transfer"
				);
			}
			assert!(
				state
					.admission
					.lock()
					.unwrap_or_else(::std::sync::PoisonError::into_inner)
					.closing,
				"the lifecycle probe observes the actual closed state"
			);
			assert_eq!(
				finished_rx.try_recv(),
				Err(::std::sync::mpsc::TryRecvError::Empty),
				"the lifecycle path passed its paused close transition"
			);
			assert_eq!(
				drops.load(::std::sync::atomic::Ordering::SeqCst),
				0,
				"the prepared value was disposed before the close transition resumed"
			);
			closing_release_tx
				.send(())
				.expect("release the lifecycle close transition");
			finished_rx
				.recv_timeout(TIMEOUT)
				.expect("the lifecycle path completed");
			if extracts {
				assert!(
					!extraction_phase.load(::std::sync::atomic::Ordering::SeqCst),
					"prepared extraction phase remained active after transfer"
				);
			}
			assert_eq!(drops.load(::std::sync::atomic::Ordering::SeqCst), 1);
			assert!(
				token.with::<PreparedRaceDrop, _>(|_| ()).is_none(),
				"the lifecycle path left prepared state accessible after closing"
			);
			cleanup.finish();
		}

		#[cfg(all(test, windows, feature = "job-object"))]
		#[test]
		fn queued_prepared_access_is_rejected_by_owner_drop() {
			assert_queued_prepared_access_is_rejected(PreparedRaceCloser::OwnerDrop);
		}

		#[cfg(all(test, windows, feature = "job-object"))]
		#[test]
		fn queued_prepared_access_is_rejected_by_consuming_extraction() {
			assert_queued_prepared_access_is_rejected(PreparedRaceCloser::ConsumingExtraction);
		}

		#[cfg(all(test, windows, feature = "job-object"))]
		#[test]
		fn queued_prepared_access_is_rejected_by_failure_cleanup() {
			assert_queued_prepared_access_is_rejected(PreparedRaceCloser::FailureCleanup);
		}

		#[cfg(all(test, windows, feature = "job-object"))]
		#[test]
		fn native_returned_child_drop_publishes_prepared_closing() {
			assert_prepared_lifecycle_path_closes(PreparedLifecyclePath::NativeDrop);
		}

		#[cfg(all(test, windows, feature = "job-object"))]
		#[test]
		fn provider_returned_child_drop_publishes_prepared_closing() {
			assert_prepared_lifecycle_path_closes(PreparedLifecyclePath::ProviderDrop);
		}

		#[cfg(all(test, windows, feature = "job-object"))]
		#[test]
		fn native_consuming_extraction_publishes_prepared_closing() {
			assert_prepared_lifecycle_path_closes(PreparedLifecyclePath::NativeExtraction);
		}

		#[cfg(all(test, windows, feature = "job-object"))]
		#[test]
		fn provider_consuming_extraction_publishes_prepared_closing() {
			assert_prepared_lifecycle_path_closes(PreparedLifecyclePath::ProviderExtraction);
		}

		#[cfg(all(test, windows, feature = "job-object"))]
		#[test]
		fn failed_spawn_cleanup_publishes_prepared_closing() {
			assert_prepared_lifecycle_path_closes(PreparedLifecyclePath::FailureCleanup);
		}

		#[cfg(all(test, windows, feature = "job-object"))]
		#[test]
		fn prepared_extraction_phase_clears_during_unwind() {
			let active = ::std::sync::atomic::AtomicBool::new(false);
			let panic = ::std::panic::catch_unwind(|| {
				let _phase = PreparedExtractionPhase::enter(&active);
				assert!(active.load(::std::sync::atomic::Ordering::SeqCst));
				panic!("injected prepared extraction failure");
			});

			assert!(panic.is_err());
			assert!(!active.load(::std::sync::atomic::Ordering::SeqCst));
		}

		#[cfg(all(test, windows, feature = "job-object"))]
		#[test]
		fn consuming_type_mismatch_restores_value_in_closed_state() {
			let drops = ::std::sync::Arc::new(::std::sync::atomic::AtomicUsize::new(0));
			let owner = PreparedChildOwner::new(Box::new(PreparedRaceDrop(
				::std::sync::Arc::clone(&drops),
			)));
			let token = owner.installed_token();

			assert!(token.take_prepared_value::<String>().is_none());
			assert!(owner.has_value(), "the mismatched box remains in owner custody");
			assert!(
				token.with::<PreparedRaceDrop, _>(|_| ()).is_none(),
				"consuming extraction closes access even when the concrete type mismatches"
			);
			drop(owner);
			assert_eq!(drops.load(::std::sync::atomic::Ordering::SeqCst), 1);
		}

		#[cfg(windows)]
		struct PreparedChildSlot<'a> {
			value: &'a mut Option<PreparedChild>,
			expected_type: ::std::any::TypeId,
		}

		/// Empty ownership slots for a child layer returned by a wrapping hook.
		///
		/// A layer keeps these slots empty while its hook runs. Process-wrap fills them only after the
		/// hook returns successfully, so failure cleanup remains outside caller-defined callbacks.
		pub struct ChildWrapperSlots<'a> {
			child: &'a mut Option<Box<dyn $childer>>,
			#[cfg(windows)]
			prepared: Option<PreparedChildSlot<'a>>,
		}

		impl<'a> ChildWrapperSlots<'a> {
			/// Describe a detached layer's empty child slot.
			pub fn new(child: &'a mut Option<Box<dyn $childer>>) -> Self {
				Self {
					child,
					#[cfg(windows)]
					prepared: None,
				}
			}

			/// Describe an empty prepared-state slot paired with concrete state type `T`.
			///
			/// Process-wrap verifies the pending value's concrete type and installs a non-owning token
			/// after the wrapping callback returns successfully. The private strong owner remains in
			/// process-wrap storage.
			#[cfg(windows)]
			#[doc(hidden)]
			pub fn with_prepared<T: ::std::any::Any + Send>(
				mut self,
				prepared: &'a mut Option<PreparedChild>,
			) -> Self {
				self.prepared = Some(PreparedChildSlot {
					value: prepared,
					expected_type: ::std::any::TypeId::of::<T>(),
				});
				self
			}
		}

		/// A detached child layer whose ownership slots process-wrap fills after its hook returns.
		///
		/// Implement this trait for a layer returned through [`PendingChildWrapper::new`]. The layer's
		/// child slot must be empty until process-wrap installs the current child.
		pub trait ChildWrapperLayer: $childer {
			/// Expose this detached layer's ownership slots.
			///
			/// Process-wrap calls this method once during installation. The child slot and any declared
			/// prepared-state token slot must be empty at that point.
			fn child_wrapper_slots(&mut self) -> ChildWrapperSlots<'_>;
		}

		/// A child layer awaiting failure-atomic installation by process-wrap.
		///
		/// Wrapping hooks return this value instead of taking ownership of the current child. If a hook
		/// succeeds, process-wrap installs that child into the layer's declared empty slot.
		pub struct PendingChildWrapper {
			layer: Box<dyn $childer>,
			install: fn(
				&mut dyn $childer,
				&mut Option<Box<dyn $childer>>,
				#[cfg(windows)] Option<&mut PreparedChildOwner>,
			) -> ::std::io::Result<()>,
		}

		impl ::std::fmt::Debug for PendingChildWrapper {
			fn fmt(&self, formatter: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
				self.layer.fmt(formatter)
			}
		}

		impl PendingChildWrapper {
			/// Return a detached child layer for failure-atomic installation.
			///
			/// `layer.child_wrapper_slots()` must expose an empty child slot. Process-wrap validates the
			/// slot and fills it after the wrapping callback has returned.
			pub fn new<L>(layer: L) -> Self
			where
				L: ChildWrapperLayer + 'static,
			{
				Self {
					layer: Box::new(layer),
					install: Self::install_layer::<L>,
				}
			}

			fn install_layer<L>(
				layer: &mut dyn $childer,
				child: &mut Option<Box<dyn $childer>>,
				#[cfg(windows)] prepared: Option<&mut PreparedChildOwner>,
			) -> ::std::io::Result<()>
			where
				L: ChildWrapperLayer + 'static,
			{
				let layer = (layer as &mut dyn ::std::any::Any)
					.downcast_mut::<L>()
					.expect("a pending child layer retains its concrete type");
				#[cfg(windows)]
				let installed_layer = (
					::std::any::TypeId::of::<L>(),
					::std::ptr::from_mut(&mut *layer).cast::<()>() as usize,
				);
				let slots = layer.child_wrapper_slots();
				if slots.child.is_some() {
					return Err(::std::io::Error::new(
						::std::io::ErrorKind::InvalidInput,
						"a pending child layer must have an empty child slot",
					));
				}
				#[cfg(windows)]
				match (prepared, slots.prepared) {
					(Some(prepared), Some(slot)) if slot.value.is_none() => {
						if prepared.state.type_id != slot.expected_type {
							return Err(::std::io::Error::new(
								::std::io::ErrorKind::InvalidInput,
								"prepared child state does not match the layer slot type",
							));
						}
						prepared.installed_layer = Some(installed_layer);
						*slot.value = Some(prepared.installed_token());
						#[cfg(all(test, feature = "job-object"))]
						if crate::windows::test_support::take_extra_prepared_owner_injection() {
							crate::windows::test_support::retain_extra_prepared_owner(Box::new(
								::std::sync::Arc::clone(&prepared.state),
							));
						}
						if !prepared.has_exclusive_custody() {
							return Err(::std::io::Error::new(
								::std::io::ErrorKind::InvalidInput,
								"prepared child state has an unexpected custody topology",
							));
						}
					}
					(Some(_), _) => {
						return Err(::std::io::Error::new(
							::std::io::ErrorKind::InvalidInput,
							"prepared child state requires an empty matching layer slot",
						));
					}
					(None, Some(slot)) if slot.value.is_some() => {
						return Err(::std::io::Error::new(
							::std::io::ErrorKind::InvalidInput,
							"a pending prepared-state slot must be empty",
						));
					}
					(None, Some(_)) => {
						return Err(::std::io::Error::new(
							::std::io::ErrorKind::InvalidInput,
							"a prepared-state slot requires matching prepared child state",
						));
					}
					(None, None) => {}
				}
				*slots.child = Some(child.take().expect("the lifecycle retains child custody"));
				Ok(())
			}

			fn install(
				&mut self,
				child: &mut Option<Box<dyn $childer>>,
				#[cfg(windows)] prepared: Option<&mut PreparedChildOwner>,
			) -> ::std::io::Result<()> {
				(self.install)(
					self.layer.as_mut(),
					child,
					#[cfg(windows)]
					prepared,
				)
			}

			fn into_child(self) -> Box<dyn $childer> {
				self.layer
			}
		}

		enum SpawnFailure {
			Error(::std::io::Error),
			Panic(Box<dyn ::std::any::Any + Send>),
		}

		impl SpawnFailure {
			fn finish<T>(self) -> ::std::io::Result<T> {
				match self {
					Self::Error(error) => Err(error),
					Self::Panic(payload) => ::std::panic::resume_unwind(payload),
				}
			}
		}

		#[derive(Debug)]
		enum ProviderTransactionState {
			Armed(Box<dyn crate::SpawnTransaction>),
			Committed(Box<dyn crate::SpawnTransaction>),
			Transferred,
		}

		impl ProviderTransactionState {
			fn new(transaction: Box<dyn crate::SpawnTransaction>) -> Self {
				Self::Armed(transaction)
			}

			fn commit(&mut self) -> ::std::io::Result<()> {
				let Self::Armed(transaction) = self else {
					unreachable!("only an armed provider transaction can commit");
				};
				transaction.commit()?;
				let Self::Armed(transaction) = ::std::mem::replace(self, Self::Transferred) else {
					unreachable!("the provider transaction remains armed until commit returns");
				};
				*self = Self::Committed(transaction);
				Ok(())
			}

			fn take_for_failure(&mut self) -> Option<(bool, Box<dyn crate::SpawnTransaction>)> {
				match ::std::mem::replace(self, Self::Transferred) {
					Self::Armed(transaction) => Some((true, transaction)),
					Self::Committed(transaction) => Some((false, transaction)),
					Self::Transferred => None,
				}
			}

			fn transfer_into(&mut self, sidecar: &mut CommittedProviderChild) {
				let Self::Committed(transaction) = ::std::mem::replace(self, Self::Transferred) else {
					unreachable!("only committed transaction residue can transfer to a child");
				};
				sidecar.install_residue(transaction);
			}
		}

		#[cfg(windows)]
		struct PreparedOwnerChild {
			// Field order is intentional: the complete child chain drops before prepared storage and
			// the disarmed duplicate process handle.
			child: Option<Box<dyn $childer>>,
			prepared: Vec<Option<PreparedChildOwner>>,
			spawn_cleanup: Option<crate::command::WindowsSpawnCleanup>,
		}

		#[cfg(windows)]
		impl PreparedOwnerChild {
			fn new(
				child: Box<dyn $childer>,
				prepared: Vec<Option<PreparedChildOwner>>,
			) -> Box<Self> {
				Self::new_with_spawn_cleanup(child, prepared, None)
			}

			fn new_with_spawn_cleanup(
				child: Box<dyn $childer>,
				prepared: Vec<Option<PreparedChildOwner>>,
				spawn_cleanup: Option<crate::command::WindowsSpawnCleanup>,
			) -> Box<Self> {
				debug_assert!(prepared.iter().any(Option::is_some) || spawn_cleanup.is_some());
				Box::new(Self {
					child: Some(child),
					prepared,
					spawn_cleanup,
				})
			}

			fn child_ref(&self) -> &dyn $childer {
				self.child
					.as_deref()
					.expect("a prepared-owner sidecar retains its child")
			}

			fn child_mut(&mut self) -> &mut dyn $childer {
				self.child
					.as_deref_mut()
					.expect("a prepared-owner sidecar retains its child")
			}

			fn take_child(&mut self) -> Option<Box<dyn $childer>> {
				self.child.take()
			}

			fn take_spawn_cleanup(&mut self) -> Option<crate::command::WindowsSpawnCleanup> {
				self.spawn_cleanup.take()
			}

			fn disarm_spawn_cleanup(&mut self) {
				if let Some(cleanup) = self.spawn_cleanup.as_mut() {
					cleanup.disarm();
				}
			}
		}

		#[cfg(windows)]
		impl ::std::fmt::Debug for PreparedOwnerChild {
			fn fmt(&self, formatter: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
				self.child_ref().fmt(formatter)
			}
		}

		#[cfg(windows)]
		impl $childer for PreparedOwnerChild {
			fn inner(&self) -> &dyn $childer {
				self.child_ref()
			}

			fn inner_mut(&mut self) -> &mut dyn $childer {
				self.child_mut()
			}

			fn into_inner(mut self: Box<Self>) -> Box<dyn $childer> {
				let removed_layer = (
					(self.child_ref() as &dyn ::std::any::Any).type_id(),
					::std::ptr::from_mut(self.child_mut()).cast::<()>() as usize,
				);
				#[cfg(feature = "job-object")]
				self.child_mut().retain_prepared_after_sidecar_removal();
				let child = self
					.take_child()
					.expect("a prepared-owner sidecar retains its child");
				let inner = child.into_inner();
				self.prepared.retain(|prepared| {
					prepared.as_ref().is_some_and(|prepared| {
						prepared.has_value() && !prepared.installed_in(removed_layer)
					})
				});
				if self.prepared.is_empty() {
					// Consuming the public sidecar ends its custody. `self` then closes any disarmed
					// duplicate handle exactly once while the extracted child moves to the caller.
					inner
				} else {
					self.child = Some(inner);
					self
				}
			}

			fn try_clone(&self) -> Option<Box<dyn $childer>> {
				None
			}

			#[cfg(windows)]
			fn process_handle(&self) -> Option<::std::os::windows::io::BorrowedHandle<'_>> {
				self.child_ref().process_handle()
			}

			#[cfg(windows)]
			fn resume_after_job_assignment(&mut self) -> Option<::std::io::Result<()>> {
				self.child_mut().resume_after_job_assignment()
			}

			$prepared_owner_contract!();
		}

		#[cfg(windows)]
		enum NativeSuccessChild {
			Plain(Box<dyn $childer>),
			Sidecar(Box<PreparedOwnerChild>),
		}

		#[cfg(windows)]
		impl NativeSuccessChild {
			fn new(
				child: Box<dyn $childer>,
				prepared: Vec<Option<PreparedChildOwner>>,
				spawn_cleanup: Option<crate::command::WindowsSpawnCleanup>,
			) -> Self {
				if prepared.iter().any(Option::is_some) || spawn_cleanup.is_some() {
					Self::Sidecar(PreparedOwnerChild::new_with_spawn_cleanup(
						child,
						prepared,
						spawn_cleanup,
					))
				} else {
					Self::Plain(child)
				}
			}

			fn child_mut(&mut self) -> &mut dyn $childer {
				match self {
					Self::Plain(child) => child.as_mut(),
					Self::Sidecar(child) => child.as_mut(),
				}
			}

			fn take_spawn_cleanup(&mut self) -> Option<crate::command::WindowsSpawnCleanup> {
				match self {
					Self::Plain(_) => None,
					Self::Sidecar(child) => child.take_spawn_cleanup(),
				}
			}

			fn disarm_spawn_cleanup(&mut self) {
				if let Self::Sidecar(child) = self {
					child.disarm_spawn_cleanup();
				}
			}

			fn into_child(self) -> Box<dyn $childer> {
				match self {
					Self::Plain(child) => child,
					Self::Sidecar(child) => child,
				}
			}
		}

		struct CommittedProviderChild {
			// Field order is intentional: child layers drop before transaction and prepared residue.
			child: Option<Box<dyn $childer>>,
			residue: ::std::sync::Arc<
				::std::sync::Mutex<Option<Box<dyn crate::SpawnTransaction>>>,
			>,
			#[cfg(windows)]
			prepared: Vec<Option<PreparedChildOwner>>,
		}

		impl CommittedProviderChild {
			fn new(
				child: Box<dyn $childer>,
				#[cfg(windows)] prepared: Vec<Option<PreparedChildOwner>>,
			) -> Box<Self> {
				Box::new(Self {
					child: Some(child),
					residue: ::std::sync::Arc::new(::std::sync::Mutex::new(None)),
					#[cfg(windows)]
					prepared,
				})
			}

			fn child_ref(&self) -> &dyn $childer {
				self.child
					.as_deref()
					.expect("a committed-provider sidecar retains its child")
			}

			fn child_mut(&mut self) -> &mut dyn $childer {
				self.child
					.as_deref_mut()
					.expect("a committed-provider sidecar retains its child")
			}

			fn take_child(&mut self) -> Option<Box<dyn $childer>> {
				self.child.take()
			}

			fn install_residue(&mut self, transaction: Box<dyn crate::SpawnTransaction>) {
				let mut residue = self
					.residue
					.lock()
					.unwrap_or_else(::std::sync::PoisonError::into_inner);
				debug_assert!(residue.is_none());
				*residue = Some(transaction);
			}

			fn take_residue(&mut self) -> Option<Box<dyn crate::SpawnTransaction>> {
				self.residue
					.lock()
					.unwrap_or_else(::std::sync::PoisonError::into_inner)
					.take()
			}
		}

		impl ::std::fmt::Debug for CommittedProviderChild {
			fn fmt(&self, formatter: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
				self.child_ref().fmt(formatter)
			}
		}

		impl $childer for CommittedProviderChild {
			fn inner(&self) -> &dyn $childer {
				self.child_ref()
			}

			fn inner_mut(&mut self) -> &mut dyn $childer {
				self.child_mut()
			}

			fn into_inner(mut self: Box<Self>) -> Box<dyn $childer> {
				#[cfg(all(windows, feature = "job-object"))]
				self.child_mut().retain_prepared_after_sidecar_removal();
				let child = self
					.take_child()
					.expect("a committed-provider sidecar retains its child");
				#[cfg(windows)]
				{
					let mut prepared = ::std::mem::take(&mut self.prepared);
					prepared.retain(|prepared| {
						prepared
							.as_ref()
							.is_some_and(PreparedChildOwner::has_value)
					});
					if !prepared.is_empty() {
						return PreparedOwnerChild::new(child, prepared);
					}
				}
				child
			}

			#[cfg(windows)]
			fn process_handle(&self) -> Option<::std::os::windows::io::BorrowedHandle<'_>> {
				self.child_ref().process_handle()
			}

			#[cfg(windows)]
			fn resume_after_job_assignment(&mut self) -> Option<::std::io::Result<()>> {
				self.child_mut().resume_after_job_assignment()
			}

			fn try_clone(&self) -> Option<Box<dyn $childer>> {
				#[cfg(windows)]
				if self.prepared.iter().any(Option::is_some) {
					return None;
				}
				self.child_ref().try_clone().map(|child| {
					Box::new(Self {
						child: Some(child),
						residue: ::std::sync::Arc::clone(&self.residue),
						#[cfg(windows)]
						prepared: self.prepared.iter().map(|_| None).collect(),
					}) as Box<dyn $childer>
				})
			}
		}

		/// An alternate transport for spawning this frontend's child contract.
		///
		/// Providers are exposed by command wrappers through [`CommandWrapper::spawn_provider`]. A
		/// command may register only one provider. The same provider and wrapper instances are reused
		/// across repeated spawn attempts, so callbacks take `&self` and must not consume persistent
		/// configuration.
		///
		/// Process-wrap invokes provider callbacks in this order: `check_available`, native-only base
		/// rejection, `validate_command`, every `pre_spawn` hook in registration order, native-only
		/// attempt rejection, `validate_attempt`, and `spawn`. After `spawn` returns a product, every
		/// `post_spawn` and child-wrapping hook runs in registration order. Process-wrap then completes
		/// the pre-commit child phase while provider rollback remains armed and preallocates the private
		/// return sidecar. It commits the transaction, installs the committed residue in that sidecar, and,
		/// on Windows, disarms the sole JobObject cleanup owner. Successful commit ends failed-spawn
		/// rollback. After the final owner succeeds, only infallible private ownership moves remain before
		/// return. Arbitrary residue destruction occurs outside the successful spawn lifecycle. `spawn_with`
		/// and `spawn_with_child` reject a registered provider instead of bypassing it.
		///
		/// A committed transaction residue must retain no armed cleanup or independent process,
		/// terminal, controller, handle, pseudoconsole, or other liveness resource.
		///
		/// Rollback, wrapper restoration, and original panic-payload preservation apply only to
		/// unwinding panics. With `panic=abort`, the process terminates before lifecycle recovery can
		/// run.
		pub trait SpawnProvider: ::std::fmt::Debug + Send + Sync + 'static {
			/// Check whether this provider is available on the current platform and runtime.
			///
			/// This runs before command validation or native-only rejection so an unsupported provider
			/// retains error precedence. It must not allocate per-spawn operating-system resources.
			fn check_available(&self) -> ::std::io::Result<()> {
				Ok(())
			}

			/// Validate immutable command and wrapper configuration before hooks run.
			///
			/// This must not allocate per-spawn operating-system resources. The provider-owning wrapper is
			/// registered but temporarily unavailable through `Command::get_wrap` during this callback;
			/// peer wrappers remain available.
			fn validate_command(&self, _command: &Command) -> ::std::io::Result<()> {
				Ok(())
			}

			/// Validate the completed portable attempt before operating-system allocation.
			///
			/// Providers should inspect `get_portable_args`, `inherits_environment`, `get_envs`, the current
			/// directory, and platform policy getters here, then reject any policy they cannot preserve.
			/// Process-wrap has already rejected an opaque attempt before this callback.
			fn validate_attempt(
				&self,
				_attempt: &SpawnAttempt,
				_command: &Command,
			) -> ::std::io::Result<()> {
				Ok(())
			}

			/// Spawn a child and return it with a fresh armed cleanup transaction.
			///
			/// The provider must honor every portable setting accepted by `validate_attempt`. Until this
			/// method returns a `ProviderProduct`, it remains responsible for cleaning up resources and any
			/// child it creates if it returns an error or an unwinding panic.
			fn spawn(
				&self,
				attempt: &mut SpawnAttempt,
				command: &Command,
			) -> ::std::io::Result<ProviderProduct>;
		}

		impl crate::command::Command<$backend> {
			fn wrapper_registry(&self) -> &WrapperRegistry {
				self.registry()
			}

			fn wrapper_registry_mut(&mut self) -> &mut WrapperRegistry {
				self.registry_mut()
			}

			/// Add a wrapper to the command.
			///
			/// This is a lazy method, and the wrapper is not actually applied until `spawn` is called.
			///
			/// Only one wrapper of a given type can be applied to a command. If `wrap` is called twice
			/// with the same type, the existing wrapper receives the newly registered wrapper through
			/// its typed `extend` hook and can merge its configuration. If the hook does nothing, the
			/// _new_ wrapper is silently discarded.
			///
			/// Returns `&mut self` for chaining.
			pub fn wrap<W: CommandWrapper + 'static>(&mut self, wrapper: W) -> &mut Self {
				let typeid = ::std::any::TypeId::of::<W>();
				let mut wrapper = Some(wrapper);
				let extant = self
					.wrapper_registry_mut()
					.wrappers
					.entry(typeid)
					.or_insert_with(|| {
						Some(Box::new(wrapper.take().unwrap()) as Box<dyn ErasedCommandWrapper>)
					});
				if let Some(wrapper) = wrapper {
					extant
						.as_mut()
						.expect("wrap() cannot run while the matching wrapper's hook is active")
						.as_any_mut()
						.downcast_mut::<W>()
						.expect("downcasting is guaranteed to succeed due to wrap()'s internals")
						.extend(wrapper);
				}

				self
			}

			#[inline]
			fn with_wrapper_at<T>(
				&mut self,
				index: usize,
				invoke: impl FnOnce(&mut dyn CommandWrapper, &Command) -> ::std::io::Result<T>,
			) -> ::std::io::Result<T> {
				let mut wrapper = self
					.wrapper_registry_mut()
					.wrappers
					.get_index_mut(index)
					.expect("wrapper indices cannot disappear during ordered hook traversal")
					.1
					.take()
					.expect("each wrapper is present when its lifecycle hook begins");

				let result = ::std::panic::catch_unwind(::std::panic::AssertUnwindSafe(|| {
					invoke(wrapper.as_command_wrapper_mut(), self)
				}));

				let slot = self
					.wrapper_registry_mut()
					.wrappers
					.get_index_mut(index)
					.expect("wrapper registrations cannot disappear while their hooks run")
					.1;
				debug_assert!(slot.is_none());
				*slot = Some(wrapper);

				match result {
					Ok(result) => result,
					Err(payload) => ::std::panic::resume_unwind(payload),
				}
			}

			fn select_spawn_provider(&self) -> ::std::io::Result<Option<usize>> {
				let mut selected = None;
				for (index, wrapper) in self.wrapper_registry().wrappers.values().enumerate() {
					let wrapper = wrapper
						.as_ref()
						.expect("provider selection runs outside wrapper lifecycle hooks");
					if wrapper
						.as_command_wrapper()
						.spawn_provider()
						.is_some()
					{
						if selected.replace(index).is_some() {
							return Err(::std::io::Error::new(
								::std::io::ErrorKind::InvalidInput,
								"multiple spawn providers are registered",
							));
						}
					}
				}
				Ok(selected)
			}

			fn with_spawn_provider_at<T>(
				&mut self,
				index: usize,
				invoke: impl FnOnce(&dyn SpawnProvider, &Command) -> ::std::io::Result<T>,
			) -> ::std::io::Result<T> {
				self.with_wrapper_at(index, |wrapper, command| {
					let provider = wrapper
						.spawn_provider()
						.expect("the selected wrapper continues to expose its spawn provider");
					invoke(provider, command)
				})
			}

			fn reject_explicit_provider(&self) -> ::std::io::Result<()> {
				if self.select_spawn_provider()?.is_some() {
					Err(::std::io::Error::new(
						::std::io::ErrorKind::InvalidInput,
						"an explicit spawner cannot bypass a registered spawn provider",
					))
				} else {
					Ok(())
				}
			}

			#[inline]
			fn run_pre_spawn(&mut self, attempt: &mut SpawnAttempt) -> ::std::io::Result<()> {
				let len = self.wrapper_registry().wrappers.len();
				for index in 0..len {
					#[cfg(feature = "tracing")]
					{
						let id = self
							.wrapper_registry()
							.wrappers
							.get_index(index)
							.expect("wrapper indices cannot disappear during ordered hook traversal")
							.0;
						::tracing::debug!(?id, "pre_spawn");
					}
					self.with_wrapper_at(index, |wrapper, command| {
						wrapper.pre_spawn(attempt, command)
					})?;
				}

				Ok(())
			}

			fn capture_io<T>(
				invoke: impl FnOnce() -> ::std::io::Result<T>,
			) -> ::std::result::Result<T, SpawnFailure> {
				match ::std::panic::catch_unwind(::std::panic::AssertUnwindSafe(invoke)) {
					Ok(Ok(value)) => Ok(value),
					Ok(Err(error)) => Err(SpawnFailure::Error(error)),
					Err(payload) => Err(SpawnFailure::Panic(payload)),
				}
			}

			#[cfg(windows)]
			#[inline]
			fn run_prepare_child(
				&mut self,
				attempt: &mut SpawnAttempt,
				child: &mut dyn $childer,
				prepared: &mut Vec<Option<PreparedChildOwner>>,
			) -> ::std::result::Result<(), SpawnFailure> {
				let len = self.wrapper_registry().wrappers.len();
				for index in 0..len {
					let value = Self::capture_io(|| {
						#[cfg(feature = "tracing")]
						{
							let id = self
								.wrapper_registry()
								.wrappers
								.get_index(index)
								.expect("wrapper indices cannot disappear during ordered hook traversal")
								.0;
							::tracing::debug!(?id, "prepare_child");
						}
						self.with_wrapper_at(index, |wrapper, command| {
							wrapper.prepare_child(attempt, child, command)
						})
					})?;
					prepared.push(value.map(PreparedChildOwner::new));
				}

				Ok(())
			}

			#[inline]
			fn run_post_spawn(
				&mut self,
				attempt: &mut SpawnAttempt,
				child: &mut dyn $childer,
			) -> ::std::result::Result<(), SpawnFailure> {
				let len = self.wrapper_registry().wrappers.len();
				for index in 0..len {
					Self::capture_io(|| {
						#[cfg(feature = "tracing")]
						{
							let id = self
								.wrapper_registry()
								.wrappers
								.get_index(index)
								.expect("wrapper indices cannot disappear during ordered hook traversal")
								.0;
							::tracing::debug!(?id, "post_spawn");
						}
						self.with_wrapper_at(index, |wrapper, command| {
							wrapper.post_spawn(attempt, child, command)
						})
					})?;
				}

				Ok(())
			}

			#[inline]
			fn run_wrap_child(
				&mut self,
				child: &mut Option<Box<dyn $childer>>,
				pending: &mut Option<PendingChildWrapper>,
				#[cfg(windows)] prepared: &mut [Option<PreparedChildOwner>],
			) -> ::std::result::Result<(), SpawnFailure> {
				let len = self.wrapper_registry().wrappers.len();
				for index in 0..len {
					let layer = Self::capture_io(|| {
						#[cfg(feature = "tracing")]
						{
							let id = self
								.wrapper_registry()
								.wrappers
								.get_index(index)
								.expect("wrapper indices cannot disappear during ordered hook traversal")
								.0;
							::tracing::debug!(?id, "wrap_child");
						}
						self.with_wrapper_at(index, |wrapper, command| {
							let child = child
								.as_deref_mut()
								.expect("process-wrap retains child custody between wrapping hooks");
							#[cfg(windows)]
							{
								wrapper.wrap_prepared_child(
									child,
									prepared[index].as_ref().map(PreparedChildOwner::view),
									command,
								)
							}
							#[cfg(not(windows))]
							{
								wrapper.wrap_child(child, command)
							}
						})
					})?;
					*pending = layer;

					if let Some(layer) = pending.as_mut() {
						Self::capture_io(|| {
							layer.install(
								child,
								#[cfg(windows)]
								prepared[index].as_mut(),
							)
						})?;
						let layer = pending
							.take()
							.expect("the installed child layer remains in custody");
						debug_assert!(child.is_none());
						*child = Some(layer.into_child());
					} else {
						#[cfg(windows)]
						if prepared[index].is_some() {
							return Err(SpawnFailure::Error(::std::io::Error::new(
								::std::io::ErrorKind::InvalidInput,
								"prepared child state requires a matching child layer",
							)));
						}
					}
				}

				Ok(())
			}

			fn finish_spawn(
				&mut self,
				attempt: &mut SpawnAttempt,
				child: Box<dyn $childer>,
			) -> ::std::io::Result<Box<dyn $childer>> {
				let mut child = Some(child);
				let mut pending = None;
				#[cfg(windows)]
				let mut prepared = Vec::with_capacity(self.wrapper_registry().wrappers.len());
				#[cfg(windows)]
				let mut cleanup = match Self::capture_io(|| {
					if !attempt.starts_suspended() {
						return Ok(None);
					}
					let handle = match child
						.as_ref()
						.expect("the native lifecycle retains child custody")
						.as_ref()
						.try_process_handle()
					{
						Some(handle) => handle,
						None => {
							let _ = child
								.as_deref_mut()
								.expect("the native lifecycle retains child custody")
								.start_kill();
							return Err(::std::io::Error::new(
								::std::io::ErrorKind::Unsupported,
								"child wrapper does not expose a Windows process handle",
							));
						}
					};
					match crate::command::WindowsSpawnCleanup::new(handle) {
						Ok(cleanup) => Ok(Some(cleanup)),
						Err(error) => {
							let _ = crate::command::terminate_process_and_wait(handle);
							Err(error)
						}
					}
				}) {
					Ok(cleanup) => cleanup,
					Err(failure) => {
						Self::cleanup_pending_child(&mut pending);
						Self::cleanup_child(&mut child);
						Self::cleanup_prepared(&mut prepared);
						return failure.finish::<Box<dyn $childer>>();
					}
				};

				let before_owner: ::std::result::Result<(), SpawnFailure> = (|| {
					#[cfg(windows)]
					self.run_prepare_child(
						attempt,
						child
							.as_deref_mut()
							.expect("the native lifecycle retains child custody"),
						&mut prepared,
					)?;
					self.run_post_spawn(
						attempt,
						child
							.as_deref_mut()
							.expect("the native lifecycle retains child custody"),
					)?;
					self.run_wrap_child(
						&mut child,
						&mut pending,
						#[cfg(windows)]
						&mut prepared,
					)?;
					Ok(())
				})();

				if let Err(failure) = before_owner {
					#[cfg(windows)]
					drop(cleanup.take());
					Self::cleanup_pending_child(&mut pending);
					Self::cleanup_child(&mut child);
					#[cfg(windows)]
					Self::cleanup_prepared(&mut prepared);
					return failure.finish::<Box<dyn $childer>>();
				}

				#[cfg(not(windows))]
				return Ok(child
					.take()
					.expect("the completed native lifecycle retains its child"));

				#[cfg(windows)]
				{
					let child = child
						.take()
						.expect("the native lifecycle retains its child before finalization");
					let mut success = NativeSuccessChild::new(
						child,
						::std::mem::take(&mut prepared),
						cleanup,
					);
					let final_owner = match Self::capture_io(|| {
						success.child_mut().finalize_spawn_before_commit()
					}) {
						Ok(owner) => owner,
						Err(failure) => {
							Self::cleanup_native_spawn_guard(&mut success);
							Self::cleanup_pending_child(&mut pending);
							Self::cleanup_native_success(success);
							return failure.finish::<Box<dyn $childer>>();
						}
					};
					attempt.prepare_for_final_owner();
					let final_result = Self::capture_io(|| {
						success
							.child_mut()
							.finalize_spawn_final_owner(final_owner)
					});
					match final_result {
						Ok(()) => {
							success.disarm_spawn_cleanup();
							Ok(success.into_child())
						}
						Err(failure) => {
							Self::cleanup_native_spawn_guard(&mut success);
							Self::cleanup_pending_child(&mut pending);
							Self::cleanup_native_success(success);
							failure.finish::<Box<dyn $childer>>()
						}
					}
				}
			}

			fn quarantine_cleanup_panic(result: ::std::thread::Result<()>) {
				if let Err(payload) = result {
					::std::mem::forget(payload);
				}
			}

			fn dispose_value<T>(value: T) {
				let disposal = ::std::panic::catch_unwind(::std::panic::AssertUnwindSafe(|| {
					drop(value);
				}));
				Self::quarantine_cleanup_panic(disposal);
			}

			fn cleanup_pending_child(pending: &mut Option<PendingChildWrapper>) {
				if let Some(pending) = pending.take() {
					Self::dispose_value(pending);
				}
			}

			fn cleanup_child(child: &mut Option<Box<dyn $childer>>) {
				if let Some(child) = child.take() {
					Self::dispose_value(child);
				}
			}

			#[cfg(windows)]
			fn cleanup_prepared(prepared: &mut [Option<PreparedChildOwner>]) {
				for prepared in prepared.iter_mut().filter_map(Option::take) {
					if let Some(value) = prepared.close_and_take() {
						Self::dispose_value(value);
					}
				}
			}

			#[cfg(windows)]
			fn cleanup_native_spawn_guard(success: &mut NativeSuccessChild) {
				if let Some(cleanup) = success.take_spawn_cleanup() {
					Self::dispose_value(cleanup);
				}
			}

			#[cfg(windows)]
			fn cleanup_native_success(mut success: NativeSuccessChild) {
				Self::cleanup_native_spawn_guard(&mut success);
				match success {
					NativeSuccessChild::Plain(child) => Self::dispose_value(child),
					NativeSuccessChild::Sidecar(mut sidecar) => {
						Self::cleanup_child(&mut sidecar.child);
						Self::cleanup_prepared(&mut sidecar.prepared);
						Self::dispose_value(sidecar);
					}
				}
			}

			fn dispose_transaction(transaction: Box<dyn crate::SpawnTransaction>) {
				Self::dispose_value(transaction);
			}

			fn rollback_transaction(mut transaction: Box<dyn crate::SpawnTransaction>) {
				let rollback = ::std::panic::catch_unwind(::std::panic::AssertUnwindSafe(|| {
					let _ = transaction.rollback();
				}));
				Self::quarantine_cleanup_panic(rollback);
				Self::dispose_transaction(transaction);
			}

			fn cleanup_failed_transaction(transaction: &mut ProviderTransactionState) {
				let Some((armed, transaction)) = transaction.take_for_failure() else {
					return;
				};
				if armed {
					Self::rollback_transaction(transaction);
				} else {
					Self::dispose_transaction(transaction);
				}
			}

			fn cleanup_provider_residue(sidecar: &mut CommittedProviderChild) {
				if let Some(transaction) = sidecar.take_residue() {
					Self::dispose_transaction(transaction);
				}
			}

			fn cleanup_provider_child(sidecar: &mut CommittedProviderChild) {
				Self::cleanup_child(&mut sidecar.child);
				#[cfg(windows)]
				Self::cleanup_prepared(&mut sidecar.prepared);
			}

			fn finish_provider_spawn(
				&mut self,
				attempt: &mut SpawnAttempt,
				product: ProviderProduct,
			) -> ::std::io::Result<Box<dyn $childer>> {
				let (child, transaction) = product.into_parts();
				let mut child = Some(child);
				let mut pending = None;
				let mut transaction = ProviderTransactionState::new(transaction);
				#[cfg(windows)]
				let mut prepared = Vec::with_capacity(self.wrapper_registry().wrappers.len());
				let before_commit: ::std::result::Result<(), SpawnFailure> = (|| {
					#[cfg(windows)]
					self.run_prepare_child(
						attempt,
						child
							.as_deref_mut()
							.expect("the provider lifecycle retains child custody"),
						&mut prepared,
					)?;
					self.run_post_spawn(
						attempt,
						child
							.as_deref_mut()
							.expect("the provider lifecycle retains child custody"),
					)?;
					self.run_wrap_child(
						&mut child,
						&mut pending,
						#[cfg(windows)]
						&mut prepared,
					)?;
					Ok(())
				})();

				if let Err(failure) = before_commit {
					Self::cleanup_failed_transaction(&mut transaction);
					Self::cleanup_pending_child(&mut pending);
					Self::cleanup_child(&mut child);
					#[cfg(windows)]
					Self::cleanup_prepared(&mut prepared);
					return failure.finish::<Box<dyn $childer>>();
				}

				#[cfg(windows)]
				let final_owner = match Self::capture_io(|| {
					child
						.as_deref_mut()
						.expect("the provider lifecycle retains child custody")
						.finalize_spawn_before_commit()
				}) {
					Ok(owner) => owner,
					Err(failure) => {
						Self::cleanup_failed_transaction(&mut transaction);
						Self::cleanup_pending_child(&mut pending);
						Self::cleanup_child(&mut child);
						Self::cleanup_prepared(&mut prepared);
						return failure.finish::<Box<dyn $childer>>();
					}
				};

				let child = child
					.take()
					.expect("the provider lifecycle retains its child before commit");
				let mut sidecar = CommittedProviderChild::new(
					child,
					#[cfg(windows)]
					::std::mem::take(&mut prepared),
				);
				if let Err(failure) = Self::capture_io(|| transaction.commit()) {
					Self::cleanup_failed_transaction(&mut transaction);
					Self::cleanup_provider_residue(&mut sidecar);
					Self::cleanup_pending_child(&mut pending);
					Self::cleanup_provider_child(&mut sidecar);
					return failure.finish::<Box<dyn $childer>>();
				}
				transaction.transfer_into(&mut sidecar);

				#[cfg(windows)]
				{
					attempt.prepare_for_final_owner();
					let final_result = Self::capture_io(|| {
						sidecar
							.child_mut()
							.finalize_spawn_final_owner(final_owner)
					});
					if let Err(failure) = final_result {
						Self::cleanup_failed_transaction(&mut transaction);
						Self::cleanup_provider_residue(&mut sidecar);
						Self::cleanup_pending_child(&mut pending);
						Self::cleanup_provider_child(&mut sidecar);
						return failure.finish::<Box<dyn $childer>>();
					}
				}

				Ok(sidecar)
			}

			fn spawn_with_provider(
				&mut self,
				provider_index: usize,
			) -> ::std::io::Result<Box<dyn $childer>> {
				self.with_spawn_provider_at(provider_index, |provider, _| {
					provider.check_available()
				})?;
				if self.is_native_only() {
					return Err(::std::io::Error::new(
						::std::io::ErrorKind::InvalidInput,
						"a spawn provider cannot use a native-only command",
					));
				}
				self.with_spawn_provider_at(provider_index, |provider, command| {
					provider.validate_command(command)
				})?;

				self.with_spawn_attempt(|command, attempt| {
					command.run_pre_spawn(attempt)?;
					if attempt.is_native_only() {
						return Err(::std::io::Error::new(
							::std::io::ErrorKind::InvalidInput,
							"a spawn provider cannot use a native-only spawn attempt",
						));
					}
					command.with_spawn_provider_at(provider_index, |provider, command| {
						provider.validate_attempt(attempt, command)
					})?;
					let product = command.with_spawn_provider_at(
						provider_index,
						|provider, command| provider.spawn(attempt, command),
					)?;
					command.finish_provider_spawn(attempt, product)
				})
			}

			/// Spawn the command, returning a child that can be interacted with.
			///
			/// With no alternate provider, this runs all `pre_spawn` hooks, spawns through the native
			/// frontend, runs all capability-level `post_spawn` hooks, then stacks all
			/// `wrap_child`s. A registered provider replaces only the native transport, commits its cleanup
			/// transaction after the same complete hook chain succeeds, and returns the child with the
			/// committed transaction residue in a private transparent layer.
			pub fn spawn(&mut self) -> ::std::io::Result<Box<dyn $childer>> {
				if let Some(provider_index) = self.select_spawn_provider()? {
					return self.spawn_with_provider(provider_index);
				}

				self.with_spawn_attempt(|command, attempt| {
					command.run_pre_spawn(attempt)?;
					let child = attempt.native_for_spawn().spawn()?;
					let child = Box::new(
						#[allow(clippy::redundant_closure_call)]
						$first_child_wrapper(child),
					) as Box<dyn $childer>;
					command.finish_spawn(attempt, child)
				})
			}

			/// Spawn the command using a custom native-child spawner function.
			///
			/// This explicit transport cannot be combined with a registered spawn provider. Without a
			/// provider, it runs the same pre-spawn, capability-level post-spawn, and child-wrapping
			/// lifecycle as [`spawn`](Self::spawn).
			///
			/// On Unix, when wrappers request built-in child setup, a successful spawner must create the
			/// returned child from the native value before replacing that value. Whenever the spawner
			/// replaces it, including before returning an error or unwinding, the displaced command must be
			/// dropped before control leaves the spawner. A replacement is discarded with a tracked attempt
			/// or retained by a native-only base. Process-wrap installs child setup before invoking the
			/// spawner and cannot apply it to a replacement which the spawner creates and immediately spawns.
			pub fn spawn_with(
				&mut self,
				spawner: impl FnOnce(&mut $command) -> ::std::io::Result<$child>,
			) -> ::std::io::Result<Box<dyn $childer>> {
				self.reject_explicit_provider()?;
				self.with_spawn_attempt(|command, attempt| {
					command.run_pre_spawn(attempt)?;
					let child = spawner(attempt.native_for_explicit_spawn())?;
					let child = Box::new(
						#[allow(clippy::redundant_closure_call)]
						$first_child_wrapper(child),
					) as Box<dyn $childer>;
					command.finish_spawn(attempt, child)
				})
			}

			/// Spawn the command using a custom boxed-child spawner function.
			///
			/// This is the spawning path for custom child implementations which do not return the
			#[doc = concat!("native [`", stringify!($child), "`] type. The closure must return a boxed [`", stringify!($childer), "`] trait object.")]
			///
			/// This explicit transport cannot be combined with a registered spawn provider. Without a
			/// provider, all `pre_spawn`, capability-level `post_spawn`, and `wrap_child` hooks run.
			///
			/// On Unix, when wrappers request built-in child setup, a successful spawner must create the
			/// returned child from the native value before replacing that value. Whenever the spawner
			/// replaces it, including before returning an error or unwinding, the displaced command must be
			/// dropped before control leaves the spawner. A replacement is discarded with a tracked attempt
			/// or retained by a native-only base. Process-wrap installs child setup before invoking the
			/// spawner and cannot apply it to a replacement which the spawner creates and immediately spawns.
			pub fn spawn_with_child(
				&mut self,
				spawner: impl FnOnce(
					&mut $command,
				) -> ::std::io::Result<Box<dyn $childer>>,
			) -> ::std::io::Result<Box<dyn $childer>> {
				self.reject_explicit_provider()?;
				self.with_spawn_attempt(|command, attempt| {
					command.run_pre_spawn(attempt)?;
					let child = spawner(attempt.native_for_explicit_spawn())?;
					command.finish_spawn(attempt, child)
				})
			}

			/// Check if a wrapper of a given type is present.
			pub fn has_wrap<W: CommandWrapper + 'static>(&self) -> bool {
				let typeid = ::std::any::TypeId::of::<W>();
				self.wrapper_registry().wrappers.contains_key(&typeid)
			}

			/// Get a reference to a wrapper of a given type.
			///
			/// This is useful for getting access to the state of a wrapper, generally from within
			/// another wrapper.
			///
			/// Returns `None` if the wrapper is not present. While a wrapper's lifecycle hook or provider
			/// callback is running, that active wrapper remains registered but is temporarily unavailable
			/// through this method; peer wrappers remain available. To merely check registration, use
			/// `has_wrap` instead.
			pub fn get_wrap<W: CommandWrapper + 'static>(&self) -> Option<&W> {
				let typeid = ::std::any::TypeId::of::<W>();
				self.wrapper_registry()
					.wrappers
					.get(&typeid)
					.and_then(Option::as_deref)
					.map(|wrapper| {
						wrapper
							.as_any()
							.downcast_ref()
							.expect("downcasting is guaranteed to succeed due to wrap()'s internals")
					})
			}
		}

		impl From<$command> for crate::command::Command<$backend> {
			fn from(command: $command) -> Self {
				Self::from_native(command)
			}
		}

		/// A trait for adding functionality to a command.
		///
		/// This trait provides extension and hook points into the lifecycle of a command. See the
		/// [crate-level documentation](crate) for an overview.
		///
		/// All methods are optional, so a minimal implementation may be:
		///
		/// ```rust,ignore
		/// #[derive(Debug)]
		/// pub struct YourWrapper;
		#[doc = concat!("impl ", stringify!(CommandWrapper), " for YourWrapper {}\n```")]
		pub trait CommandWrapper: ::std::fmt::Debug + Send + Sync {
			/// Called on a first instance if a second of the same type is added.
			///
			/// Only one wrapper of a given type can exist within a command at a time. By default, later
			/// registrations are discarded. In some cases it is useful to merge their configuration
			/// instead. This method is called on the stored wrapper with the newly registered wrapper of
			/// the same concrete type.
			///
			/// Because `other` is `Self`, implementations can inspect or move its type-specific fields
			/// directly without downcasting.
			///
			/// Default implementation: no-op.
			fn extend(&mut self, _other: Self)
			where
				Self: Sized,
			{
			}

			/// Called before the command is spawned, to mutate this attempt as needed.
			///
			/// Hooks run in registration order and stop at the first error or unwinding panic. Mutations to
			/// an attempt copied from a tracked command apply to that spawn only. A native-only base instead
			/// retains native mutations when process-wrap restores it after the lifecycle.
			///
			/// Calling `SpawnAttempt::native_mut`, directly or through `stdin`, `stdout`, or `stderr`, makes a
			/// tracked attempt opaque. A registered portable provider rejects it after all pre-spawn hooks
			/// and before `validate_attempt` or operating-system allocation. Portable policy setters remain
			/// representable; a transport may apply their policy only after every hook has run and in the
			/// order required by the platform. On Unix, recurring native escapes from a native-only base can
			/// retain inactive dispatcher callbacks because the native API does not expose callback insertion
			/// or command ownership; use portable attempt methods for recurring configuration.
			///
			/// The `command` reference provides read-only access to peer wrappers and persistent base
			/// configuration. The active wrapper remains registered but is temporarily unavailable through
			/// `Command::get_wrap`.
			///
			/// Default implementation: no-op.
			fn pre_spawn(
				&mut self,
				_attempt: &mut SpawnAttempt,
				_command: &Command,
			) -> ::std::io::Result<()> {
				Ok(())
			}

			/// Prepare Windows child state which must exist before public post-spawn hooks run.
			///
			/// Return `Some(Box::new(state))` for a concrete `state: T`. Process-wrap keeps that value in
			/// private, non-cloneable pending custody through later hooks. The matching
			/// [`CommandWrapper::wrap_prepared_child`] callback receives only immutable type metadata,
			/// never the value or a custody handle.
			///
			/// The complete safe installation pattern is: return `T` here; have the detached layer store
			/// an empty `Option<PreparedChild>`; confirm `PreparedChildRef::is::<T>()` while wrapping;
			/// return that layer through `PendingChildWrapper::new`; and expose its slot with
			/// `ChildWrapperSlots::with_prepared::<T>`. Process-wrap validates the type and exact owner
			/// topology before filling the slot. Once installed, the layer may use
			/// `PreparedChild::with::<T, _>` for immutable access.
			///
			/// A prepared type which itself exposes interior ownership transfer or independently clonable
			/// native resources can deliberately extend their lifetime; that is part of the type's own
			/// contract. Process-wrap does not expose a cloneable custody token or mutable pre-install
			/// accessor which creates such an escape.
			#[doc(hidden)]
			#[cfg(windows)]
			fn prepare_child(
				&mut self,
				_attempt: &mut SpawnAttempt,
				_child: &mut dyn $childer,
				_command: &Command,
			) -> ::std::io::Result<Option<Box<dyn ::std::any::Any + Send>>> {
				Ok(None)
			}

			/// Called after any transport spawns a child, but before the child is wrapped.
			///
			/// Hooks run in registration order and stop at the first error or unwinding panic. The child is
			/// exposed through the frontend's object-safe capability trait, so it may be a terminal custom or
			/// provider child with no native child value. The transport has already created it: changing
			/// command settings on `attempt` here cannot configure that child.
			///
			/// On the provider path, an error or unwinding panic triggers best-effort transaction rollback. Native
			/// transports do not promise equivalent child cleanup on every platform.
			///
			/// Default implementation: no-op.
			fn post_spawn(
				&mut self,
				_attempt: &mut SpawnAttempt,
				_child: &mut dyn $childer,
				_command: &Command,
			) -> ::std::io::Result<()> {
				Ok(())
			}

			/// Called to describe a child layer after a transport creates the child.
			///
			/// Wrappers run in registration order. The current child remains borrowed from process-wrap while
			/// this callback runs. To add a layer, return a [`PendingChildWrapper`] containing a
			/// [`ChildWrapperLayer`] whose ownership slots are empty; process-wrap installs the current child
			/// only after the callback has returned successfully. Returning `None` leaves the child unchanged.
			/// This custody protocol keeps child destruction outside the callback boundary when a later hook
			/// fails. `.wrap(Foo).wrap(Bar)` still produces an outer `Bar(Foo(child))` layer.
			///
			/// On the provider path, an error or unwinding panic triggers best-effort transaction rollback.
			///
			/// Default implementation: no-op (returns no layer).
			fn wrap_child(
				&mut self,
				_child: &mut dyn $childer,
				_command: &Command,
			) -> ::std::io::Result<Option<PendingChildWrapper>> {
				Ok(None)
			}

			/// Describe a child layer for state returned by `prepare_child`.
			///
			/// `prepared` is a non-cloneable metadata view. It can establish presence and concrete type but
			/// cannot expose, mutate, replace, or move the pending value. A matching layer declares an
			/// empty typed slot; process-wrap alone creates its installed `PreparedChild` handle after this
			/// callback succeeds.
			#[doc(hidden)]
			#[cfg(windows)]
			fn wrap_prepared_child(
				&mut self,
				child: &mut dyn $childer,
				prepared: Option<PreparedChildRef<'_>>,
				command: &Command,
			) -> ::std::io::Result<Option<PendingChildWrapper>> {
				debug_assert!(
					prepared.is_none(),
					"the default child preparation does not produce wrapper state"
				);
				self.wrap_child(child, command)
			}

			/// Expose an alternate spawn provider implemented by this wrapper.
			///
			/// If this returns `Some` during provider selection, it must continue returning `Some` for every
			/// callback in that spawn lifecycle. The returned provider must refer to the same persistent
			/// provider state. Only one registered wrapper may expose a provider.
			///
			/// During a provider callback, this owning wrapper remains registered but is temporarily
			/// unavailable through `Command::get_wrap`; peer wrappers remain available.
			///
			/// Default implementation: no provider.
			fn spawn_provider(&self) -> Option<&dyn SpawnProvider> {
				None
			}
		}
	};
}

pub(crate) use Wrap;
