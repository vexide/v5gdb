use std::process::{ExitCode, Termination};

pub fn test<F, R>(run: F)
where
    F: FnOnce() -> R,
    R: Termination,
{
    println!("::test:status=run");
    std::panic::set_hook(Box::new(|mesg| {
        println!("{mesg}");
        println!("::test:status=panic");
        std::process::exit(1);
    }));
    let output = run().report();

    if output == ExitCode::SUCCESS {
        println!("::test:status=done");
    } else {
        println!("::test:status=error");
    }
}

macro_rules! test_harness {
    ($test:ident) => {
        #[vexide::main(banner(enabled = false))]
        async fn main(p: Peripherals) {
            $crate::common::test(|| $test(p));
        }
    };
}
pub(crate) use test_harness;
