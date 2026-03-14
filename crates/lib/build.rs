fn main() {
  println!("cargo:rustc-cdylib-link-arg=-Wl,-install_name,@rpath/libmacmon.dylib");
}
