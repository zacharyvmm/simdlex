#[derive(Debug, Clone)]
pub struct SimdMapping {
    pub tlo: [u8; 16],
    pub thi: [u8; 16],
    pub masks: Vec<(u8, u8)>, // (character, assigned_mask)
}

pub fn generate_simd_tables(structural: &[u8], whitespace: &[u8]) -> Option<SimdMapping> {
    // Combine targets and keep track of which are structural
    let total_len = structural.len() + whitespace.len();
    let mut targets = Vec::with_capacity(total_len);
    let mut is_struct = Vec::with_capacity(total_len);

    for &c in structural {
        targets.push(c);
        is_struct.push(true);
    }
    for &c in whitespace {
        targets.push(c);
        is_struct.push(false);
    }

    let mut assigned_masks = vec![0; total_len];
    let mut tlo = [0u8; 16];
    let mut thi = [0u8; 16];

    if solve(
        0,
        &targets,
        &is_struct,
        &mut assigned_masks,
        &mut tlo,
        &mut thi,
        0,
        0,
    ) {
        let masks = targets.into_iter().zip(assigned_masks).collect();
        Some(SimdMapping { tlo, thi, masks })
    } else {
        None
    }
}

fn solve(
    idx: usize,
    targets: &[u8],
    is_struct: &[bool],
    assigned: &mut [u8],
    tlo: &mut [u8; 16],
    thi: &mut [u8; 16],
    struct_used: u8,
    white_used: u8,
) -> bool {
    // Base Case: All characters successfully assigned
    if idx == targets.len() {
        return true;
    }

    let c = targets[idx];
    let lo_nibble = (c & 0x0F) as usize;
    let hi_nibble = ((c >> 4) & 0x0F) as usize;
    let is_structural = is_struct[idx];

    // Try all 8 possible power-of-2 masks
    for shift in 0..8 {
        let mask = 1 << shift;

        // Rule 2: Domain Firewall (Structural != Whitespace)
        if is_structural && (white_used & mask) != 0 {
            continue;
        }
        if !is_structural && (struct_used & mask) != 0 {
            continue;
        }

        // Save current state for backtracking
        let old_tlo = tlo[lo_nibble];
        let old_thi = thi[hi_nibble];

        // Apply mask
        tlo[lo_nibble] |= mask;
        thi[hi_nibble] |= mask;
        assigned[idx] = mask;

        // Pruning: Check for false positives and mask collisions immediately
        let mut ok = true;
        for b in 0..=255u8 {
            let val = tlo[(b & 0x0F) as usize] & thi[((b >> 4) & 0x0F) as usize];

            if val != 0 {
                if let Some(target_idx) = targets.iter().position(|&x| x == b) {
                    if target_idx <= idx {
                        // Already assigned (including current char): must match exactly
                        if val != assigned[target_idx] {
                            ok = false;
                            break;
                        }
                    } else {
                        // Not assigned yet: val MUST be a power of 2 because
                        // this future target will be forced to include this bit.
                        if !val.is_power_of_two() {
                            ok = false;
                            break;
                        }
                    }
                } else {
                    // Non-target character generated a non-zero value: False Positive!
                    ok = false;
                    break;
                }
            }
        }

        // If valid so far, recurse to the next character
        if ok {
            let next_struct = struct_used | if is_structural { mask } else { 0 };
            let next_white = white_used | if !is_structural { mask } else { 0 };

            if solve(
                idx + 1,
                targets,
                is_struct,
                assigned,
                tlo,
                thi,
                next_struct,
                next_white,
            ) {
                return true; // Fast exit on first valid solution found
            }
        }

        // Backtrack: restore state
        tlo[lo_nibble] = old_tlo;
        thi[hi_nibble] = old_thi;
    }

    false // No valid mask found for this character in this branch
}

// --- Execution & Presentation ---

fn main() {
    // let structural = [b'[', b']', b'{', b'}', b',', b':'];
    // let whitespace = [b'\t', b'\n', b'\r', b' '];

    let structural = [b'"', b'\'', b'<', b'>', b'-', b'='];
    let whitespace = [b'\t', b'\n', b'\r', b' '];

    println!("Deriving SIMD tables in pure Rust...\n");

    if let Some(mapping) = generate_simd_tables(&structural, &whitespace) {
        println!("=== Lookup Tables ===");
        println!("lo: {:?}", mapping.tlo);
        println!("hi: {:?}", mapping.thi);

        println!("\n=== Extracted Masks ===");
        for (c, mask) in mapping.masks {
            let display_char = match c {
                0x09 => "\\t".to_string(),
                0x0a => "\\n".to_string(),
                0x0d => "\\r".to_string(),
                0x20 => "' '".to_string(),
                _ => (c as char).to_string(),
            };
            println!("Char {:>3} ({c}) : Mask {}", display_char, mask);
        }
    } else {
        println!("UNSAT: No valid mapping exists.");
    }
}
