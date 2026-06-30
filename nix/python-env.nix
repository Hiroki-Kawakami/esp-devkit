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
    version = "5.3.1";
    url = "https://files.pythonhosted.org/packages/76/ac/d2016cf6b3709d0e0166f45f84bc6e2d717757b5f59020ccb34de08d1b9b/esptool-5.3.1.tar.gz";
    hash = "sha256-EleB825qLQjEhFJKRfNAaUZ1Note7q2dDLIbIDSpHZg=";
    deps = with py; [ bitstring cryptography pyserial reedsolo pyyaml intelhex rich-click click ];
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
    version = "1.16.0";
    url = "https://files.pythonhosted.org/packages/d0/9c/17e2134e8573837af47631d4dd27ba3001aa557dfe63a890df4aa2dad006/esp_coredump-1.16.0.tar.gz";
    hash = "sha256-1Jog+q8+sXh4/7NRtYHrfjLUeTPU0KFIkAxck2HoNUs=";
    deps = with py; [ construct pygdbmi ] ++ [ esptool ];
  };

  esp-idf-kconfig = mkPypiPkg {
    pname = "esp-idf-kconfig";
    version = "3.11.1";
    url = "https://files.pythonhosted.org/packages/ea/d2/d8916259ee827aeea9df1765d7b9de66c406032fceefb4701330717891e5/esp_idf_kconfig-3.11.1.tar.gz";
    hash = "sha256-AYLN+ZSTSbRdLeDwhWtxtkq9I0S1E6JBq7KcFHyV7BA=";
    deps = with py; [ rich pyparsing textual ];
  };

  esp-idf-monitor = mkPypiPkg {
    pname = "esp-idf-monitor";
    version = "1.9.0";
    url = "https://files.pythonhosted.org/packages/7c/86/64a8984759506fbbbfce14ec981ec736420aeb79ffc964166a507e3be065/esp_idf_monitor-1.9.0.tar.gz";
    hash = "sha256-DDjaDD04PUtjBYY7jfjqbgkwOtWm1bqSznGzHxz3AM4=";
    deps = with py; [ pyserial pyelftools ] ++ [ esp-coredump esp-idf-panic-decoder ];
  };

  esp-idf-nvs-partition-gen = mkPypiPkg {
    pname = "esp-idf-nvs-partition-gen";
    version = "0.1.9";
    url = "https://files.pythonhosted.org/packages/7b/78/572dcca160714a4e60bee0db591f327c63df3bad3ecda22b410ed8ea5299/esp_idf_nvs_partition_gen-0.1.9.tar.gz";
    hash = "sha256-Q6ObVLJDGJ6REJpOmZaMhk8y4g4Ne5wzBSu9JqXxaQ8=";
    deps = with py; [ cryptography ];
  };

  esp-idf-diag = mkPypiPkg {
    pname = "esp-idf-diag";
    version = "0.2.0";
    url = "https://files.pythonhosted.org/packages/5d/e8/ebb81a1a297dfc2c1d94dce2a412b1e956049baed8ddcaf0d61cc26a2e7a/esp_idf_diag-0.2.0.tar.gz";
    hash = "sha256-g6/6mSLnq56eEWg/NQc1b1kDhfc1pDUyRC0tkwGk6KA=";
    deps = with py; [ pyyaml rich ];
  };

  idf-component-manager = mkPypiPkg {
    pname = "idf-component-manager";
    version = "3.0.3";
    url = "https://files.pythonhosted.org/packages/86/4b/4fd210417a4c16ec3da56d4143ffe3503fe77f6791ee299f02114d708340/idf_component_manager-3.0.3.tar.gz";
    hash = "sha256-UU8T9fx/Ssl0+wtD0wQq7Y9zjxkhnaXUDPZ/ibrVX0c=";
    deps = with py; [
      psutil click colorama pyparsing ruamel-yaml requests requests-file requests-toolbelt
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
    inherit esptool esp-coredump esp-idf-kconfig esp-idf-monitor
            esp-idf-nvs-partition-gen esp-idf-diag idf-component-manager
            esp-idf-panic-decoder pyclang tree-sitter-c;
  };

  pythonEnv = python.withPackages (ps: with ps; [
    setuptools packaging click pyserial cryptography pyparsing pyelftools
    construct rich rich-click psutil freertos-gdb tree-sitter esp-idf-size
  ] ++ (builtins.attrValues custom));
in {
  inherit pythonEnv;
  inherit (custom) esptool;
}
