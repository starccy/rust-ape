//! The interface name/index map, through shim/ifname.c. cosmo has none of
//! these calls.
//! Each host resolves the index by a different mechanism, so what is asserted
//! is that the three calls agree with each other rather than any particular
//! number.

use std::collections::HashSet;

use nix::net::if_::{if_indextoname, if_nameindex, if_nametoindex};

fn main() {
    let interfaces = if_nameindex().expect("if_nameindex");
    let listed: Vec<(u32, String)> = interfaces
        .iter()
        .map(|i| (i.index(), i.name().to_string_lossy().into_owned()))
        .collect();
    assert!(!listed.is_empty(), "if_nameindex listed no interfaces at all");

    println!("if_nameindex() -> {} interfaces", listed.len());
    for (index, name) in &listed {
        assert_ne!(*index, 0, "{name} was listed with index 0");
        assert!(!name.is_empty(), "index {index} was listed with an empty name");
        println!("  {index:<6} {name}");
    }

    let mut seen_index = HashSet::new();
    let mut seen_name = HashSet::new();
    for (index, name) in &listed {
        assert!(seen_index.insert(index), "index {index} listed twice");
        assert!(seen_name.insert(name), "name {name} listed twice");
    }

    for (index, name) in &listed {
        let got = if_nametoindex(name.as_str())
            .unwrap_or_else(|e| panic!("if_nametoindex({name}) failed: {e}"));
        assert_eq!(got, *index, "if_nametoindex({name}) disagrees with if_nameindex");

        let back = if_indextoname(*index)
            .unwrap_or_else(|e| panic!("if_indextoname({index}) failed: {e}"));
        let back = back.to_string_lossy();
        assert_eq!(back, *name, "if_indextoname({index}) disagrees with if_nameindex");
    }
    println!("all {} round-tripped name -> index -> name", listed.len());

    let absent = if_nametoindex("no_such_iface");
    assert!(absent.is_err(), "if_nametoindex(no_such_iface) answered {absent:?}");

    // nix checks if_indextoname's return against -1 rather than NULL, so a
    // failed call arrives as Ok("") instead of Err. Either way an index
    // nothing owns must not resolve to a real interface.
    let unused = listed.iter().map(|(i, _)| *i).max().unwrap() + 1000;
    let stale = if_indextoname(unused).map_or_else(
        |_| String::new(),
        |name| name.to_string_lossy().into_owned(),
    );
    assert!(stale.is_empty(), "if_indextoname({unused}) resolved to {stale:?}");
    println!("absent name reports an error, unused index resolves to nothing");

    println!("\nif_name_index ok");
}
