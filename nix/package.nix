{
  lib,
  rustPlatform,
  makeWrapper,
}:

rustPlatform.buildRustPackage {
  pname = "tcomp";
  version = (lib.importTOML ../Cargo.toml).package.version;

  src = lib.fileset.toSource {
    root = ../.;
    fileset = lib.fileset.unions [
      ../Cargo.toml
      ../Cargo.lock
      ../crates
      ../src
      ../web
    ];
  };

  cargoLock.lockFile = ../Cargo.lock;

  nativeBuildInputs = [ makeWrapper ];

  postInstall = ''
    mkdir -p $out/share/tcomp
    cp -r web $out/share/tcomp/web
    wrapProgram $out/bin/tcomp --set-default TCOMP_WEB_DIR $out/share/tcomp/web
  '';

  meta = {
    description = "Take your terminals on the go: mirror a terminal session into the browser";
    homepage = "https://github.com/flodurorg/tcomp";
    license = lib.licenses.mit;
    mainProgram = "tcomp";
    platforms = lib.platforms.unix;
  };
}
