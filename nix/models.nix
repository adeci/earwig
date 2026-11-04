# Every curated model from models.toml as a pinned fixed-output download, so
# NixOS users can point `services.earwig.model` at the store and run offline.
# `default` is the catalog default.
{ lib, fetchurl }:
let
  catalog = (lib.importTOML ../crates/earwig-core/models.toml).model;
  fetch =
    m:
    fetchurl {
      name = m.file;
      url = "https://huggingface.co/${m.repo}/resolve/${m.revision}/${m.file}";
      inherit (m) sha256;
      passthru = { inherit (m) id repo revision; };
      meta = {
        description = m.name;
        homepage = "https://huggingface.co/${m.repo}";
      };
    };
  models = lib.listToAttrs (map (m: lib.nameValuePair m.id (fetch m)) catalog);
  defaults = lib.filter (m: m.default or false) catalog;
in
assert lib.assertMsg (lib.length defaults == 1) "models.toml must mark exactly one default model";
models // { default = models.${(lib.head defaults).id}; }
