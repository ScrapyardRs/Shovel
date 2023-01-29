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

pub trait System: Sized + Send + Sync + 'static {
    type CreationDetails;

    fn bootstrap(details: Self::CreationDetails) {
        Self::bootstrap_with_runtime(SystemRuntime::default(), details);
    }

    fn bootstrap_with_runtime(
        runtime: SystemRuntime,
        details: Self::CreationDetails,
    ) -> std::thread::JoinHandle<()> {
        let mut system = Self::create(details);
        let handle = (runtime.system_thread_builder)()
            .spawn(move || {
                (runtime.tokio_worker_builder)()
                    .build()
                    .unwrap()
                    .block_on(async move {
                        loop {
                            if system.tick().await.is_err() {
                                break;
                            }
                        }
                    })
            })
            .unwrap();
        handle
    }

    fn create(details: Self::CreationDetails) -> Self;

    fn tick(&mut self) -> PinnedLivelyResult<()>;
}
