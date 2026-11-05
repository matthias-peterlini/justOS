//! Snake. Arrows (or WASD) steer, Q or Esc quits.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::collections::VecDeque;
use justrt::console::{self, color::*, Key};
use justrt::println;

justrt::main!(main);

struct Rng(u64);

impl Rng {
    fn next(&mut self, n: usize) -> usize {
        self.0 = self.0.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        ((self.0 >> 33) % n as u64) as usize
    }
}

fn put(x: usize, y: usize, s: &str) {
    console::goto(x, y);
    console::write(s);
}

fn main(_: &str) -> i32 {
    let (cols, rows) = console::size();
    let (w, h) = ((cols / 2).clamp(10, 40), (rows - 4).clamp(8, 24));
    let (ox, oy) = ((cols - w * 2 - 2) / 2, 2);
    console::clear();
    put(ox, 0, &alloc::format!("{ACCENT}snake{RESET}  {DIM}arrows steer · q quits{RESET}"));
    // Border.
    let mut line = alloc::string::String::from(DIM);
    for _ in 0..w * 2 + 2 {
        line.push('─');
    }
    line.push_str(RESET);
    put(ox, oy - 1, &line);
    put(ox, oy + h, &line);
    for y in 0..h {
        put(ox, oy + y, &alloc::format!("{DIM}│{RESET}"));
        put(ox + w * 2 + 1, oy + y, &alloc::format!("{DIM}│{RESET}"));
    }

    let mut rng = Rng(justrt::time::uptime_ms() ^ 0x9E37_79B9_7F4A_7C15);
    let mut snake: VecDeque<(usize, usize)> = VecDeque::new();
    for i in 0..4 {
        snake.push_back((w / 2 - i, h / 2));
    }
    let (mut dx, mut dy) = (1isize, 0isize);
    let cell = |x: usize, y: usize, s: &str| put(ox + 1 + x * 2, oy + y, s);
    let place_food = |rng: &mut Rng, snake: &VecDeque<(usize, usize)>| loop {
        let f = (rng.next(w), rng.next(h));
        if !snake.contains(&f) {
            return f;
        }
    };
    let mut food = place_food(&mut rng, &snake);
    for &(x, y) in &snake {
        cell(x, y, &alloc::format!("{GREEN}██{RESET}"));
    }
    cell(food.0, food.1, &alloc::format!("{RED}●{RESET}"));
    let mut score = 0;
    let mut speed = 140;
    loop {
        put(ox + w * 2 - 10, 0, &alloc::format!("{WHITE}score {score:>3}{RESET}"));
        let deadline = justrt::time::uptime_ms() + speed;
        while let Some(k) = console::poll_key(deadline.saturating_sub(justrt::time::uptime_ms())) {
            let (nx, ny) = match k.key {
                Key::Up | Key::Char('w') => (0, -1),
                Key::Down | Key::Char('s') => (0, 1),
                Key::Left | Key::Char('a') => (-1, 0),
                Key::Right | Key::Char('d') => (1, 0),
                Key::Char('q') | Key::Escape => {
                    console::clear();
                    println!("score: {score}");
                    return 0;
                }
                _ => (dx, dy),
            };
            if (nx, ny) != (-dx, -dy) {
                (dx, dy) = (nx, ny);
            }
            if justrt::time::uptime_ms() >= deadline {
                break;
            }
        }
        let (hx, hy) = *snake.front().unwrap();
        let nx = hx as isize + dx;
        let ny = hy as isize + dy;
        if nx < 0 || ny < 0 || nx >= w as isize || ny >= h as isize || snake.contains(&(nx as usize, ny as usize)) {
            put(ox + w - 4, oy + h / 2, &alloc::format!("{RED}game over{RESET}"));
            console::goto(0, oy + h + 1);
            println!("score: {score}");
            return 0;
        }
        let head = (nx as usize, ny as usize);
        snake.push_front(head);
        cell(head.0, head.1, &alloc::format!("{GREEN}██{RESET}"));
        if head == food {
            score += 1;
            speed = (speed - 4).max(60);
            food = place_food(&mut rng, &snake);
            cell(food.0, food.1, &alloc::format!("{RED}●{RESET}"));
        } else if let Some((tx, ty)) = snake.pop_back() {
            cell(tx, ty, "  ");
        }
    }
}
