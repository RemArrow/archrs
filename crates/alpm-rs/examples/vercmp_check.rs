use std::cmp::Ordering;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let r = alpm_rs::vercmp(&args[1], &args[2]);
    let n = match r {
        Ordering::Less => -1,
        Ordering::Equal => 0,
        Ordering::Greater => 1,
    };
    println!("{n}");
}
