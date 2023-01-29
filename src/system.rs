use drax::PinnedLivelyResult;

pub type TokioBuilder = fn() -> tokio::runtime::Builder;
pub type SystemThreadBuilder = fn() -> std::thread::Builder;

pub struct SystemRuntime {
    system_thread_builder: SystemThreadBuilder,
    tokio_worker_builder: TokioBuilder,
}

impl SystemRuntime {
    pub fn new(
        system_thread_builder: SystemThreadBuilder,
        tokio_worker_builder: TokioBuilder,
    ) -> Self {
        Self {
            system_thread_builder,
            tokio_worker_builder,
        }
    }
}

impl Default for SystemRuntime {
    fn default() -> Self {
        Self::new(
            || std::thread::Builder::new(),
            || {
                let mut builder = tokio::runtime::Builder::new_current_thread();
                builder.enable_all();
                builder
            },
        )
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub enum TickResult {
    Continue,
    Stop,
}

pub trait System: Sized + Send + Sync + 'static {
    type CreationDetails;
    type SplitOff;

    fn bootstrap(details: Self::CreationDetails) {
        Self::bootstrap_with_runtime(SystemRuntime::default(), details);
    }

    fn bootstrap_with_runtime(
        runtime: SystemRuntime,
        details: Self::CreationDetails,
    ) -> (Self::SplitOff, std::thread::JoinHandle<()>) {
        let (mut system, split_off) = Self::create(details);
        let handle = (runtime.system_thread_builder)()
            .spawn(move || {
                (runtime.tokio_worker_builder)()
                    .build()
                    .unwrap()
                    .block_on(async move {
                        loop {
                            if matches!(system.tick(), TickResult::Stop) {
                                break;
                            }
                        }
                    })
            })
            .unwrap();
        (split_off, handle)
    }

    fn create(details: Self::CreationDetails) -> (Self, Self::SplitOff);

    fn tick(&mut self) -> TickResult;
}
