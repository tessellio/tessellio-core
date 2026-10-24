//! LCG example.

use tessellio_core::scene::Lcg;

fn main() {
    let mut lcg = Lcg::new(42);
    println!("next: {}", lcg.next_u32());
    println!("below 10: {}", lcg.below(10));
}
