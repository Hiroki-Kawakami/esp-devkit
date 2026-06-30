{ pkgs }:

let
  python = pkgs.python3;
  py = python.pkgs;

  mkPypiPkg = { pname, version, url, hash, deps ? [] }:
    py.buildPythonPackage {
      inherit pname version;
      pyproject = true;
      src = pkgs.fetchurl { inherit url hash; };
      build-system = [ py.setuptools ];
      dependencies = deps;
      doCheck = false;
      pythonImportsCheck = [];
    };

  esptool = mkPypiPkg {
    pname = "esptool";
    version = "4.8.1";
    url = "https://files.pythonhosted.org/packages/5c/6b/3ce9bb7f36bdef3d6ae71646a1d3b7d59826a478f3ed8a783a93a2f8f537/esptool-4.8.1.tar.gz";
    hash = "sha256-3E7ya2WeGo3LAZFHwOptlJgLNN6Z++CRIceUHIslRTE=";
    deps = with py; [ bitstring cryptography ecdsa pyserial reedsolo pyyaml intelhex argcomplete ];
  };

  esp-idf-panic-decoder = mkPypiPkg {
    pname = "esp-idf-panic-decoder";
    version = "1.5.0";
    url = "https://files.pythonhosted.org/packages/da/f1/e4d6170a51e15afd27660992fbd2a4728cf401750ebb388b1ce035d95025/esp_idf_panic_decoder-1.5.0.tar.gz";
    hash = "sha256-ewk/ehul/f135CqWBIe1pDP2Dc2r84zJgXbykfw6iPs=";
    deps = with py; [ pyelftools pyparsing ];
  };

  esp-coredump = mkPypiPkg {
    pname = "esp-coredump";
    version = "1.15.0";
    url = "https://files.pythonhosted.org/packages/e2/4e/4ba12832cceda0ca8203f62d7c50b224c47ead4f50da0d48c4a421e52ac6/esp_coredump-1.15.0.tar.gz";
    hash = "sha256-X/pAVmB9rMbVFL3oDTato3KTGiiyA+n1suVwOm7qAs4=";
    deps = with py; [ construct pygdbmi ] ++ [ esptool ];
  };

  esp-idf-kconfig = mkPypiPkg {
    pname = "esp-idf-kconfig";
    version = "2.5.4";
    url = "https://files.pythonhosted.org/packages/d5/67/65dea4c47a1c395b33558ddf6e175970a949b0278c9279494ef3c927d78c/esp_idf_kconfig-2.5.4.tar.gz";
    hash = "sha256-3UH3QyqDiC3ppHc9PLOI+a7zGscXixYjL9FnyNz1VEY=";
    deps = with py; [ ];
  };

  esp-idf-monitor = mkPypiPkg {
    pname = "esp-idf-monitor";
    version = "1.9.0";
    url = "https://files.pythonhosted.org/packages/7c/86/64a8984759506fbbbfce14ec981ec736420aeb79ffc964166a507e3be065/esp_idf_monitor-1.9.0.tar.gz";
    hash = "sha256-DDjaDD04PUtjBYY7jfjqbgkwOtWm1bqSznGzHxz3AM4=";
    deps = with py; [ pyserial pyelftools ] ++ [ esp-coredump esp-idf-panic-decoder ];
  };

  esp-idf-size = mkPypiPkg {
    pname = "esp-idf-size";
    version = "1.7.1";
    url = "https://files.pythonhosted.org/packages/f7/a0/c8b13d7b27daec1e88a8d6c5f8f3cf6f4eae795c70f19fb70c4bc37ce943/esp_idf_size-1.7.1.tar.gz";
    hash = "sha256-labUYKJukzADWq8eHCXM83FgVJdWIUMgzMqEBNl9zBs=";
    deps = with py; [ pyyaml rich ];
  };

  esp-idf-nvs-partition-gen = mkPypiPkg {
    pname = "esp-idf-nvs-partition-gen";
    version = "0.1.9";
    url = "https://files.pythonhosted.org/packages/7b/78/572dcca160714a4e60bee0db591f327c63df3bad3ecda22b410ed8ea5299/esp_idf_nvs_partition_gen-0.1.9.tar.gz";
    hash = "sha256-Q6ObVLJDGJ6REJpOmZaMhk8y4g4Ne5wzBSu9JqXxaQ8=";
    deps = with py; [ cryptography ];
  };

  idf-component-manager = mkPypiPkg {
    pname = "idf-component-manager";
    version = "2.2.2";
    url = "https://files.pythonhosted.org/packages/0e/06/792f79a71a9302234870dc206e040bdfc73c4015310c74637354809d3402/idf_component_manager-2.2.2.tar.gz";
    hash = "sha256-HKOJIThQ05khSBhDzVjX+cF2hWrBivNZXmQZWBpIGvc=";
    deps = with py; [
      click colorama pyparsing ruamel-yaml requests requests-file requests-toolbelt
      tqdm jsonref pydantic pydantic-core pydantic-settings typing-extensions truststore
    ];
  };

  pyclang = mkPypiPkg {
    pname = "pyclang";
    version = "0.6.3";
    url = "https://files.pythonhosted.org/packages/84/5a/246d89413dfb3fbd24185e0baf2697be3eb6ef5ce7f0dc22f32fcc4ce47b/pyclang-0.6.3.tar.gz";
    hash = "sha256-CxFRwZhiGfQcuRpXcyQQlejSKD/qqPlHyYnGWE/E1Wo=";
    deps = [];
  };

  tree-sitter-c = mkPypiPkg {
    pname = "tree-sitter-c";
    version = "0.24.2";
    url = "https://files.pythonhosted.org/packages/a6/c9/3834f3d9278251aea7312274971bc4c45b17aec2490fd4b884d93bd7019a/tree_sitter_c-0.24.2.tar.gz";
    hash = "sha256-FihYTfApm1o0CqY/jme2yXyRUX9S+n56TFV+QK2zMKk=";
    deps = with py; [ tree-sitter ];
  };

  custom = {
    inherit esptool esp-coredump esp-idf-kconfig esp-idf-monitor esp-idf-size
            esp-idf-nvs-partition-gen idf-component-manager esp-idf-panic-decoder
            pyclang tree-sitter-c;
  };

  pythonEnv = python.withPackages (ps: with ps; [
    setuptools packaging click pyserial cryptography pyparsing pyelftools
    construct rich psutil freertos-gdb
  ] ++ (builtins.attrValues custom));
in {
  inherit pythonEnv;
  inherit (custom) esptool;
}
