use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicU64, Ordering};

use fieldcad_core::{ComponentTypeId, Dimension, DistanceProbeId, ObjectId, PluginId, PropertyId};
use fieldcad_expressions::{
    ConstantDefinition, ConstantId, ConstantScope, EvaluationPlan, ExpressionDocument,
    PropertyBinding, PropertyBindingSchema, PropertyTarget, ValueProvider,
};

struct CountingAllocator;

static ALLOCATIONS: AtomicU64 = AtomicU64::new(0);

unsafe impl GlobalAlloc for CountingAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        ALLOCATIONS.fetch_add(1, Ordering::Relaxed);
        unsafe { System.alloc(layout) }
    }

    unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
        unsafe { System.dealloc(pointer, layout) }
    }
}

#[global_allocator]
static ALLOCATOR: CountingAllocator = CountingAllocator;

struct Distance(std::cell::Cell<f64>);

impl ValueProvider for Distance {
    fn distance(&self, probe: DistanceProbeId) -> Option<f64> {
        (probe == DistanceProbeId::new(3)).then(|| self.0.get())
    }
}

#[test]
fn warmed_candidate_evaluation_allocates_nothing() {
    let target = PropertyTarget {
        object: ObjectId::new(1),
        component: ComponentTypeId::new(PluginId::new("alloc-test").unwrap(), "body").unwrap(),
        property: PropertyId::new("length").unwrap(),
    };
    let constants = (0..64)
        .map(|index| ConstantDefinition {
            id: ConstantId::new(index),
            scope: ConstantScope::Document,
            name: format!("v{index}"),
            source: if index == 0 {
                "distance.3".into()
            } else {
                format!("doc.v{} + 1 m", index - 1).into()
            },
            revision: None,
            provenance: None,
        })
        .collect();
    let document = ExpressionDocument {
        constants,
        bindings: vec![PropertyBinding {
            target: target.clone(),
            source: "doc.v63".into(),
        }],
    };
    let mut plan = EvaluationPlan::compile(&document, |candidate| {
        (candidate == &target).then_some(PropertyBindingSchema {
            dimension: Dimension::LENGTH,
            live_binding: true,
        })
    })
    .unwrap();
    let distance = Distance(std::cell::Cell::new(1.0));
    plan.evaluate_candidate(&distance).unwrap();
    plan.adopt_candidate();

    let before = ALLOCATIONS.load(Ordering::Relaxed);
    for iteration in 0..1_000 {
        distance.0.set(1.0 + f64::from(iteration) * 0.001);
        plan.evaluate_candidate(&distance).unwrap();
        plan.adopt_candidate();
    }
    let allocated = ALLOCATIONS.load(Ordering::Relaxed) - before;
    assert_eq!(allocated, 0, "steady-state expression evaluation allocated");
}
