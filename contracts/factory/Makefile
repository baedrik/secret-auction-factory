all:
	RUSTFLAGS='-C link-arg=-s' cargo build --release --target wasm32-unknown-unknown
	cp ./target/wasm32-unknown-unknown/release/*.wasm ./contract.wasm
	## The following line is not necessary, may work only on linux (extra size optimization)
	# wasm-opt -Os ./contract.wasm -o ./contract.wasm
	cat ./contract.wasm | gzip -9 > ./contract.wasm.gz

clean:
	cargo clean
	-rm -f ./contract.wasm ./contract.wasm.gz

find-floating-points:
	cargo build --release --target wasm32-unknown-unknown --locked
	twiggy paths ./target/wasm32-unknown-unknown/release/*.wasm > find_floats_twiggy.txt
	wasm2wat ./target/wasm32-unknown-unknown/release/*.wasm | grep -B 20 -P 'f(64|32)' > find_floats_grep.txt
