use std::env;
fn eval_a(i: i64, j: i64) -> f64 {
    1.0 / (((i + j) * (i + j + 1) / 2 + i + 1) as f64)
}
fn multiply_av(v: &[f64], n: usize) -> Vec<f64> {
    let mut out = vec![0.0f64; n];
    for i in 0..n {
        let mut s = 0.0;
        for j in 0..n {
            s += v[j] * eval_a(i as i64, j as i64);
        }
        out[i] = s;
    }
    out
}
fn multiply_atv(v: &[f64], n: usize) -> Vec<f64> {
    let mut out = vec![0.0f64; n];
    for i in 0..n {
        let mut s = 0.0;
        for j in 0..n {
            s += v[j] * eval_a(j as i64, i as i64);
        }
        out[i] = s;
    }
    out
}
fn multiply_atav(v: &[f64], n: usize) -> Vec<f64> {
    multiply_atv(&multiply_av(v, n), n)
}
fn main() {
    let args: Vec<String> = env::args().collect();
    let n: usize = if args.len() > 1 {
        args[1].parse().unwrap()
    } else {
        5500
    };
    let mut u = vec![1.0f64; n];
    let mut v = vec![0.0f64; n];
    for _ in 0..10 {
        v = multiply_atav(&u, n);
        u = multiply_atav(&v, n);
    }
    let mut vbv = 0.0;
    let mut vv = 0.0;
    for i in 0..n {
        vbv += u[i] * v[i];
        vv += v[i] * v[i];
    }
    println!("{:.9}", (vbv / vv).sqrt());
}
