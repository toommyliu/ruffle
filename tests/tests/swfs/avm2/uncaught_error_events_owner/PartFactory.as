package {
    import flash.utils.getDefinitionByName;

    public class PartFactory {
        public static function make():* {
            return new (getDefinitionByName("Part") as Class)();
        }
    }
}
