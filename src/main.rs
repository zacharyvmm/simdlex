fn main() {
    let html = br#"
        <div class="text-red-500 font-bold">
            <span id='x' class='hover:bg-blue-500 p-4'></span>
            <button class=rounded-lg></button>
        </div>
    "#;

    for span in simdlex::extract_class_spans(html) {
        let value =
            std::str::from_utf8(&html[span.value_start as usize..span.value_end as usize]).unwrap();
        println!("{value}");
    }
}
