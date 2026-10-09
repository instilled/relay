//! The graph's parts: the nodes wired to each other, whichever way, one part each. Two flows
//! with nothing between them are two parts, laid out apart.

/// Each of `n` nodes' part, by the `edges` between them: numbered in the order `reading` first
/// comes to one of each part's nodes.
pub fn of(n: usize, edges: impl Iterator<Item = (usize, usize)>, reading: impl Iterator<Item = usize>) -> Vec<usize> {
    let mut near: Vec<Vec<usize>> = vec![vec![]; n];
    for (a, b) in edges {
        near[a].push(b);
        near[b].push(a);
    }
    let mut part = vec![usize::MAX; n];
    let mut count = 0;
    for start in reading.chain(0..n) {
        if part[start] != usize::MAX {
            continue;
        }
        part[start] = count;
        let mut todo = vec![start];
        while let Some(v) = todo.pop() {
            for &w in &near[v] {
                if part[w] == usize::MAX {
                    part[w] = count;
                    todo.push(w);
                }
            }
        }
        count += 1;
    }
    part
}

#[cfg(test)]
mod tests {
    #[test]
    fn what_is_wired_together_is_one_part_numbered_as_read() {
        // 0-1-2 and 3-4, 5 alone; read from 3.
        let part = super::of(6, [(0, 1), (2, 1), (3, 4)].into_iter(), [3, 0, 5].into_iter());
        assert_eq!(part, [1, 1, 1, 0, 0, 2]);
    }
}
