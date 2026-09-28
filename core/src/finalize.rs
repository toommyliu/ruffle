use gc_arena::collect::Trace;
use gc_arena::{Collect, Finalization, Gc, GcWeak};

pub fn resurrect<'gc>(fc: &Finalization<'gc>, value: &(impl Collect<'gc> + ?Sized)) -> bool {
    struct Resurrect<'a, 'gc> {
        fc: &'a Finalization<'gc>,
        any_dead: bool,
    }

    impl<'gc> Trace<'gc> for Resurrect<'_, 'gc> {
        fn trace_gc(&mut self, gc: Gc<'gc, ()>) {
            if Gc::is_dead(self.fc, gc) {
                Gc::resurrect(self.fc, gc);
                self.any_dead = true;
            }
        }

        fn trace_gc_weak(&mut self, _gc: GcWeak<'gc, ()>) {}
    }

    let mut resurrect = Resurrect {
        fc,
        any_dead: false,
    };
    value.trace(&mut resurrect);
    resurrect.any_dead
}
