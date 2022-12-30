use super::*;

#[test]
fn adds_file_subprograms_implicitly() {
    check_code_with_no_diagnostics(
        "
use std.textio.text;

package pkg is
end package;

package body pkg is
  procedure proc is
    file f : text;
  begin
    file_open(f, \"foo.txt\");
    assert not endfile(f);
    file_close(f);
  end procedure;
end package body;
",
    );
}

#[test]
fn adds_to_string_for_integer_types() {
    check_code_with_no_diagnostics(
        "
package pkg is
  type type_t is range 0 to 1;
  alias my_to_string is to_string[type_t, return string];
end package;
",
    );
}

#[test]
fn adds_to_string_for_array_types() {
    check_code_with_no_diagnostics(
        "
package pkg is
  type type_t is array (natural range 0 to 1) of integer;
  alias my_to_string is to_string[type_t, return string];
end package;
",
    );
}

#[test]
fn no_error_for_duplicate_alias_of_implicit() {
    check_code_with_no_diagnostics(
        "
package pkg is
  type type_t is array (natural range 0 to 1) of integer;
  alias alias_t is type_t;
  -- Should result in no error for duplication definiton of for example TO_STRING
end package;
",
    );
}

#[test]
fn deallocate_is_defined_for_access_type() {
    check_code_with_no_diagnostics(
        "
package pkg is
  type arr_t is array (natural range <>) of character;
  type ptr_t is access arr_t;

  procedure theproc is
      variable theptr: ptr_t;
  begin
      deallocate(theptr);
  end procedure;
end package;
",
    );
}
