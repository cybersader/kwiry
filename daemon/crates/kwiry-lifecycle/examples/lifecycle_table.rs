//! Print the generated §5.8 block for `docs/roadmap/ig2a-governed-semantic-design.md`.

fn main() {
    println!("{}", kwiry_lifecycle::table::render());
}
